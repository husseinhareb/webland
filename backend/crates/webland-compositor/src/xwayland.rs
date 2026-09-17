//! X11 clients, by way of `XWayland`.
//!
//! Steam, and every other application that never grew a Wayland backend, talks
//! X11 and nothing else. `XWayland` is an X server that renders into Wayland
//! surfaces, so the pixels arrive on the same path everything else uses and the
//! browser never learns that a window came from X.
//!
//! What is X-shaped is the window management. An X client places, sizes and
//! stacks its own windows by asking the window manager, so the compositor has to
//! run one — [`X11Wm`] — and answer. Webland's answers are short, because the
//! browser does the layout: a window gets the size the browser is showing, a
//! configure request is granted, and stacking is not the compositor's business.

use std::process::Stdio;

use smithay::reexports::calloop::{EventLoop, LoopHandle};
use smithay::reexports::wayland_server::Display;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::{Reorder, ResizeEdge, XwmId};
use smithay::xwayland::{X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler};

use crate::Webland;

/// Start the X server and attach a window manager to it.
///
/// Spawning is asynchronous — the server has to come up and claim a display
/// number before anything can be told about it — so the socket is inserted into
/// the event loop and the manager starts when it reports ready. A failure is a
/// warning rather than an error: a desktop without X clients is a working
/// desktop, and refusing to start at all over one would be worse.
///
/// Returns whether there is now an X server on its way. `false` means nothing
/// was started and nothing will report ready, which is what [`wait_ready`] has
/// to know: waiting out the timeout for a server that was never spawned is five
/// seconds of frozen desktop on every machine without `xwayland` installed.
pub(crate) fn start(loop_handle: &LoopHandle<'static, Webland>, dh: &DisplayHandle) -> bool {
    let (xwayland, client) = match XWayland::spawn(
        dh,
        None,
        std::iter::empty::<(String, String)>(),
        // Listen on the abstract socket too: an X client that was told `DISPLAY`
        // and nothing else finds the server through `@/tmp/.X11-unix/X<n>`.
        true,
        Stdio::null(),
        Stdio::null(),
        |_| {},
    ) {
        Ok(pair) => pair,
        Err(err) => {
            tracing::warn!(%err, "XWayland did not start; X11 clients will not run");
            return false;
        }
    };

    let handle = loop_handle.clone();
    let inserted =
        loop_handle.insert_source(
            xwayland,
            move |event, (), state: &mut Webland| match event {
                XWaylandEvent::Ready {
                    x11_socket,
                    display_number,
                } => match X11Wm::start_wm(handle.clone(), x11_socket, client.clone()) {
                    Ok(wm) => {
                        tracing::info!(
                            display = display_number,
                            "XWayland up; X11 clients can run"
                        );
                        state.xwm = Some(wm);
                        state.xdisplay = Some(display_number);
                    }
                    Err(err) => {
                        tracing::warn!(%err, "XWayland is up but its window manager is not");
                    }
                },
                XWaylandEvent::Error => {
                    tracing::warn!("XWayland stopped; X11 clients will not run");
                    // `xwm` is deliberately left alone: see `xwm_state`.
                    state.xwayland_gone = true;
                    state.xdisplay = None;
                }
            },
        );
    if let Err(err) = inserted {
        tracing::warn!(%err, "could not watch XWayland");
        return false;
    }
    true
}

impl Webland {
    /// The rectangle an X window is put at: the whole of what the browser shows.
    ///
    /// X has no notion of a compositor that will place the window later, so it
    /// must be given coordinates now. They are the origin, because where the
    /// window actually sits is browser-side state the compositor never learns —
    /// the same reason `xdg_toplevel` windows are configured at a size and told
    /// nothing about position.
    fn x11_rect(&self) -> Rectangle<i32, Logical> {
        let (width, height) = self.size;
        Rectangle::from_size((width.max(1), height.max(1)).into())
    }

    /// Take the window into the set the browser is shown, once it has a surface
    /// to show. An X window exists before its `wl_surface` does, and a window
    /// with no surface has no pixels for any of this to be about.
    fn adopt_x11(&mut self, window: &X11Surface) {
        if window.wl_surface().is_none() || self.x11.iter().any(|known| known == window) {
            return;
        }
        self.x11.push(window.clone());
    }

    fn drop_x11(&mut self, window: &X11Surface) {
        self.x11.retain(|known| known != window);
    }

    /// The X window a surface belongs to, if it came from X at all.
    pub(crate) fn x11_for(&self, surface: &WlSurface) -> Option<&X11Surface> {
        self.x11
            .iter()
            .find(|window| window.wl_surface().as_ref() == Some(surface))
    }

    /// Configure every X window at the browser's new size, as a resize does for
    /// `xdg_toplevel` clients.
    pub(crate) fn resize_x11(&mut self) {
        let rect = self.x11_rect();
        for window in &self.x11 {
            if let Err(err) = window.configure(rect) {
                tracing::warn!(%err, "could not resize an X11 window");
            }
        }
    }
}

impl XWaylandShellHandler for Webland {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    /// The `wl_surface` an X window draws into has arrived.
    ///
    /// The two halves are made separately and matched up here, in either order:
    /// a client can map the X window before its surface exists, so this is the
    /// second of the two places a window becomes showable.
    fn surface_associated(&mut self, _xwm: XwmId, _surface: WlSurface, window: X11Surface) {
        if window.is_mapped() {
            self.adopt_x11(&window);
        }
    }
}

impl XwmHandler for Webland {
    /// Only ever reached from the window manager's own event source, so there is
    /// an `X11Wm` by construction — provided nothing else takes it away.
    ///
    /// Which is why `xwm` is set once and never cleared. An X server that dies
    /// leaves events already queued behind it, and each of them comes back
    /// through here: clearing the handle when the server went away turned a dead
    /// X server into a dead compositor. A stale `X11Wm` talks to a closed socket
    /// instead, which fails the way every other X call here already can.
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().expect("xwm handler without an xwm")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    /// An X client is asking for its window to appear.
    ///
    /// Granted, at the size the browser is showing — an X client picks its own
    /// size and would otherwise open at whatever it last remembered, which on a
    /// desktop whose size the browser owns is a guess.
    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        tracing::info!(title = %window.title(), class = %window.class(), "new X11 window mapped");
        if let Err(err) = window.configure(self.x11_rect()) {
            tracing::warn!(%err, "could not size an X11 window");
        }
        if let Err(err) = window.set_mapped(true) {
            tracing::warn!(%err, "could not map an X11 window");
            return;
        }
        self.adopt_x11(&window);
    }

    /// A menu, a tooltip, a drag icon: a window X places itself and the window
    /// manager is told about rather than asked.
    ///
    /// Shown like any other, at the size and place the client chose. The browser
    /// gets it as a surface of its own — unanchored, because X gives a position
    /// on the screen rather than a parent to hang from.
    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.adopt_x11(&window);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.drop_x11(&window);
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.drop_x11(&window);
    }

    /// An X client asking to move or resize itself.
    ///
    /// The size is granted and the position is not: where a window sits is the
    /// browser's, and a client that could re-anchor itself would jump out from
    /// under the chrome drawn around it.
    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _x: Option<i32>,
        _y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let mut rect = self.x11_rect();
        #[allow(clippy::cast_possible_wrap)]
        {
            if let Some(w) = w {
                rect.size.w = (w as i32).max(1);
            }
            if let Some(h) = h {
                rect.size.h = (h as i32).max(1);
            }
        }
        if let Err(err) = window.configure(rect) {
            tracing::warn!(%err, "could not answer an X11 configure request");
        }
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
    }

    /// Both gestures belong to the browser: it draws the titlebar that would
    /// start a move and the grip that would start a resize, and it acts on them
    /// itself without the compositor hearing about it.
    fn resize_request(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _button: u32,
        _edge: ResizeEdge,
    ) {
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}

    fn disconnected(&mut self, _xwm: XwmId) {
        // `xwm` is deliberately left alone: see `xwm_state`.
        self.xwayland_gone = true;
        self.xdisplay = None;
        self.x11.clear();
    }
}

/// Block until the X server reports its display number, or give up.
///
/// Startup only, and bounded: everything the browser launches needs `DISPLAY` in
/// its environment from the first one, and a desktop that waited forever on an X
/// server that never came would never start at all.
///
/// Both loops have to turn here. The X server is a Wayland client of this
/// compositor as well as a calloop source, and it does not report ready until it
/// has finished talking to the display — so pumping only the event loop waits
/// for a message the X server is waiting on the compositor to let it send.
/// Only called when [`start`] reported an X server on its way, and it gives up
/// the moment one reports that it died — so a machine with no `xwayland` at all
/// waits for nothing.
pub(crate) fn wait_ready(
    event_loop: &mut EventLoop<'static, Webland>,
    display: &mut Display<Webland>,
    state: &mut Webland,
) {
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    while state.xdisplay.is_none() && !state.xwayland_gone && std::time::Instant::now() < deadline {
        if event_loop
            .dispatch(Some(std::time::Duration::from_millis(5)), state)
            .is_err()
            || display.dispatch_clients(state).is_err()
            || display.flush_clients().is_err()
        {
            break;
        }
    }
    if state.xdisplay.is_none() && !state.xwayland_gone {
        tracing::warn!("XWayland did not come up in time; X11 clients will not run");
    }
}

/// How long to wait for the X server at startup. Generous: it forks, opens its
/// sockets and connects back, and a loaded machine can take a moment over it.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl Webland {
    /// Tell the X windows which of them the browser just raised.
    ///
    /// Keyboard focus itself is not set here — `XWayland` follows the Wayland
    /// seat for that. This is the `_NET_WM_STATE_FOCUSED` hint, which is what an
    /// X client reads to know whether to draw itself active or dimmed, and it
    /// has no way to work that out from the Wayland side.
    pub(crate) fn activate_x11(&self, focused: Option<&ObjectId>) {
        for window in &self.x11 {
            let is_focused = window.wl_surface().map(|s| s.id()).as_ref() == focused;
            if window.is_activated() != is_focused
                && let Err(err) = window.set_activated(is_focused)
            {
                tracing::warn!(%err, "could not tell an X11 window it is focused");
            }
        }
    }
}
