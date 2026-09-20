//! Wayland compositor for Webland.
//!
//! Built on [`smithay`]. Wayland-first, but not Wayland-only: X11 clients run
//! through `XWayland`, which lives in [`xwayland`] and reaches the rest of this
//! module as surfaces like any other.
//!
//! Phase 1 ([`run_winit`]): render mapped surfaces into a window on the host
//! desktop, so a real Wayland client can connect and be seen. No headless
//! output, no browser, no streaming yet — that is Phase 2.
//!
//! Adapted from smithay's `minimal` example, routed through
//! `smithay::reexports::*` and kept free of `unsafe` (the workspace denies it,
//! which is why child environments are set per-`Command` rather than via the
//! now-`unsafe` `std::env::set_var`).

// This crate is a thin integration layer over smithay, whose API forces casts
// and unwraps that pedantic would otherwise flag on our side.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::default_trait_access,
    clippy::needless_pass_by_value,
    // `Webland` is the compositor's state, and the flags in it are independent
    // facts about independent protocols. Folding them into an enum would say
    // they are alternatives, which they are not.
    clippy::struct_excessive_bools
)]

/// Re-exported so downstream crates pin one Wayland stack.
pub use smithay;

use std::collections::{HashMap, HashSet};
use std::os::unix::io::OwnedFd;
use std::sync::Arc;

use webland_core::{Rect, Size, SurfaceId};
use webland_protocol::{
    Anchor, ClientMessage, Codec, InputEvent, Press, ServerMessage, SurfaceCreated, SurfaceFrame,
    WindowRequest,
};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::gbm::GbmDevice;
pub mod apps;
pub mod encode;
pub mod spawn;
mod xwayland;

use smithay::backend::allocator::{Buffer, Fourcc, Modifier};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::input::{
    Axis, AxisSource, ButtonState, InputEvent as BackendInputEvent, KeyState, KeyboardKeyEvent,
    Keycode,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{
    CommitCounter, RendererSurfaceStateUserData, draw_render_elements, on_commit_buffer_handler,
    with_renderer_surface_state,
};
use smithay::backend::renderer::{
    Bind, Color32F, ExportMem, Frame, ImportDma, Offscreen, Renderer,
};
use smithay::backend::winit::{self, WinitEvent};
use smithay::input::keyboard::{FilterResult, KeyboardHandle, XkbConfig};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent, PointerHandle, RelativeMotionEvent,
};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{
    ClientData, ClientId, DisconnectReason, ObjectId,
};
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::protocol::wl_seat;
use smithay::reexports::wayland_server::protocol::wl_surface::{self, WlSurface};
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket, Resource};
use smithay::reexports::winit::platform::pump_events::PumpStatus;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial, Transform};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes, TraversalAction,
    get_children, with_surface_tree_downward,
};
use smithay::wayland::dmabuf::{
    DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier, get_dmabuf,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    request_data_device_client_selection, set_data_device_focus, set_data_device_selection,
};
use smithay::wayland::selection::primary_selection::{
    request_primary_client_selection, set_primary_focus, set_primary_selection,
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler,
    XdgShellState,
};
use smithay::wayland::shell::xdg::decoration::{
    XdgDecorationHandler, XdgDecorationState,
};
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::pointer_constraints::{
    PointerConstraint, PointerConstraintsHandler, PointerConstraintsState, with_pointer_constraint,
};
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::reexports::calloop::EventLoop;
use smithay::wayland::output::OutputHandler;
use smithay::wayland::xwayland_shell::XWaylandShellState;
use smithay::xwayland::{X11Surface, X11Wm, XWaylandClientData};
use smithay::wayland::shm::{ShmHandler, ShmState, with_buffer_contents};
use smithay::{
    delegate_compositor, delegate_cursor_shape, delegate_data_device, delegate_dmabuf,
    delegate_output, delegate_pointer_constraints, delegate_primary_selection,
    delegate_relative_pointer, delegate_seat, delegate_shm, delegate_xdg_decoration,
    delegate_xdg_shell, delegate_xwayland_shell,
};

/// Compositor state. Holds the protocol globals and the seat; owns everything a
/// Wayland client interacts with.
#[derive(Debug)]
pub struct Webland {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    dmabuf_state: DmabufState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    primary_selection_state: PrimarySelectionState,
    _relative_pointer_state: RelativePointerManagerState,
    _pointer_constraints_state: PointerConstraintsState,
    pointer_location: Point<f64, Logical>,
    /// The browser's window, told to clients as a monitor. See [`browser_output`].
    output: Output,
    /// The `xwayland_shell_v1` global, which is how `XWayland` tells the
    /// compositor that a `wl_surface` is the one an X window draws into.
    xwayland_shell_state: XWaylandShellState,
    /// The window manager for the X server, once it is up. `None` without one,
    /// which is a desktop that runs Wayland clients and no X ones.
    xwm: Option<X11Wm>,
    /// The X display number, for `DISPLAY` in what the browser launches.
    xdisplay: Option<u32>,
    /// Set once the X server has reported that it died. Startup reads it so a
    /// server that failed on the way up is not waited out to the timeout.
    xwayland_gone: bool,
    /// Mapped X windows. Kept apart from `xdg_shell_state` because X windows are
    /// not `xdg_toplevel`s: they are captured and streamed the same way, but
    /// everything the compositor says back to one is said in X.
    x11: Vec<X11Surface>,
    seat: Seat<Self>,
    /// Set by the browser on connect: send whole surfaces on the next frame,
    /// because a joiner has nothing for a damage rectangle to land on.
    keyframe: bool,
    /// The surface the browser last raised, which is where input goes.
    focus: Option<SurfaceId>,
    /// The size to configure toplevels at, as the browser last reported it.
    size: (i32, i32),
    /// Send the launcher's list on the next pass.
    announce_applications: bool,
    /// Toplevels that asked the compositor to decorate them, and so must not be
    /// given a second titlebar by the shell. See [`Webland::decorates_itself`].
    server_decorated: HashSet<ObjectId>,
    /// Move, maximize and minimize asked for by a client's own titlebar, waiting
    /// to go to the browser, which owns where windows sit.
    requests: Vec<(ObjectId, WindowRequest)>,
    /// Open popups — menus, tooltips, combobox lists — oldest first, so a
    /// submenu always follows the menu it came from.
    popups: Vec<PopupSurface>,
    /// Needed to hand the seat a selection the browser owns.
    dh: DisplayHandle,
    /// The browser's clipboard, which is what clients are given when they paste.
    clipboard: String,
    /// Text copied by a client, on its way to the browser. A pipe read cannot
    /// happen inline — the client writes when it feels like it — so the read
    /// runs on a thread and the answer arrives here.
    copied: std::sync::mpsc::Sender<String>,
    pastes: std::sync::mpsc::Receiver<String>,
    /// What the pointer currently looks like, as a CSS cursor keyword.
    cursor: String,
    /// Tell the browser about the cursor on the next pass.
    announce_cursor: bool,
}

impl BufferHandler for Webland {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl CompositorHandler for Webland {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    /// The X server is a client the compositor never inserted — smithay does it,
    /// with client data of smithay's own — so there are two places the state can
    /// live and neither is a safe assumption.
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(data) = client.get_data::<XWaylandClientData>() {
            return &data.compositor_state;
        }
        &client
            .get_data::<ClientState>()
            .expect("client inserted without compositor state")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        // This also moves the commit's damage into the surface's renderer
        // state, converted to buffer coordinates — which is where
        // `damage_since` reads it, so nothing else needs doing here.
        on_commit_buffer_handler::<Self>(surface);
    }
}

impl XdgShellHandler for Webland {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Headless has no output, so clients have no size to render at and pick
        // a small default. Tell them the browser's, which `ClientMessage::Resize`
        // keeps current; `WEBLAND_SIZE` is only the value before one arrives.
        let (width, height) = self.size;
        tracing::info!(width, height, "new xdg toplevel mapped");
        // Without this a client knows the output exists but not that it is on
        // it, which is the half of the answer GTK reads the scale from.
        self.output.enter(surface.wl_surface());
        surface.with_pending_state(|state| {
            state.size = Some((width, height).into());
            state.states.set(xdg_toplevel::State::Activated);
            // Tiled on all four edges, which is a lie about the layout told for
            // its side effect: a self-decorating client drops its drop shadow
            // and its rounded corners when an edge is tiled, because neither
            // makes sense against a neighbour. Without it the client's shadow
            // margin is part of the buffer, and a margin that is transparent to
            // the client is opaque black once encoded — a black band around
            // every GTK window. Tiling compositors use exactly this trick.
            for edge in [
                xdg_toplevel::State::TiledLeft,
                xdg_toplevel::State::TiledRight,
                xdg_toplevel::State::TiledTop,
                xdg_toplevel::State::TiledBottom,
            ] {
                state.states.set(edge);
            }
        });
        surface.send_configure();
    }

    /// A menu, a tooltip, a combobox list: a surface the client places itself,
    /// against the window that opened it.
    ///
    /// The positioner does the placing — anchor rectangle, gravity and offset,
    /// all of it relative to the parent's window geometry — and smithay works
    /// the rectangle out. Nothing here constrains it to the screen: the browser
    /// knows where the parent window actually sits and the compositor does not.
    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
        });
        // An error means the popup is already mapped, which is not ours to
        // configure a second time.
        if surface.send_configure().is_err() {
            return;
        }
        self.popups.push(surface);
    }

    /// A popup asking for the pointer means a menu: it stays up until something
    /// outside it is clicked. That click arrives as a focus change from the
    /// browser, so there is nothing to grab here — see [`dismiss_popups`].
    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    /// The gestures a self-decorating client makes on its own titlebar.
    ///
    /// The shell owns window position, stacking and which windows are hidden, so
    /// none of this can be answered here: it is forwarded to the browser, which
    /// does the same thing it does when its own chrome is clicked. Queued rather
    /// than sent, because the transport is only in reach once a frame goes out.
    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        self.requests
            .push((surface.wl_surface().id(), WindowRequest::Move));
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.requests
            .push((surface.wl_surface().id(), WindowRequest::Maximize));
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.requests
            .push((surface.wl_surface().id(), WindowRequest::Unmaximize));
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        self.requests
            .push((surface.wl_surface().id(), WindowRequest::Minimize));
    }

    /// The client moved a popup that is already up — a menu that would have run
    /// off the screen, usually.
    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
        });
        surface.send_repositioned(token);
    }
}

/// Webland draws every window's chrome itself, in the browser (see
/// `frontend/src/desktop`), so clients must not draw their own: a client that
/// falls back to client-side decorations puts a second titlebar, with a second
/// set of buttons, inside the one the shell already drew.
///
/// A preference is all a client gets to express here. The mode is the
/// compositor's to choose, and this one has only one answer.
impl XdgDecorationHandler for Webland {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.server_decorated.insert(toplevel.wl_surface().id());
        Self::decorate_server_side(&toplevel);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        self.server_decorated.insert(toplevel.wl_surface().id());
        Self::decorate_server_side(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.server_decorated.insert(toplevel.wl_surface().id());
        Self::decorate_server_side(&toplevel);
    }
}

impl Webland {
    /// Whether this toplevel draws its own titlebar.
    ///
    /// A client that never creates a decoration object has no way to be told the
    /// compositor decorates, and xdg-decoration says to assume it decorates
    /// itself — which is exactly what GTK does, since it does not implement the
    /// protocol at all. The shell skips its chrome for these, or the window
    /// wears two titlebars.
    fn decorates_itself(&self, surface: &WlSurface) -> bool {
        // An X client never implements `xdg-decoration`; it says the same thing
        // through `_MOTIF_WM_HINTS`, which is what `is_decorated` reads — and it
        // reads it as "this window is client-side decorated", already the way
        // round this asks. Not negated: an X client says nothing about motif
        // hints far more often than not, and that silence means it wants the
        // window manager's frame, which is the shell's to draw.
        if let Some(window) = self.x11_for(surface) {
            return window.is_decorated();
        }
        !self.server_decorated.contains(&surface.id())
    }

    /// Tell a toplevel the compositor is drawing its decorations.
    fn decorate_server_side(toplevel: &ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_configure();
    }
}

impl ShmHandler for Webland {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for Webland {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    // ponytail: accepts without importing. The renderer lives in `run_headless`,
    // not in this state, and the formats we advertise came from that same
    // renderer — so a buffer that fails here would be a surprise. A failed
    // import at capture time just skips the frame. Import here (and hold the
    // renderer in `Webland`) if clients ever start seeing silent black windows.
    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        _dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let _ = notifier.successful::<Self>();
    }
}

impl SeatHandler for Webland {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|s| self.dh.get_client(s.id()).ok());
        set_data_device_focus(&self.dh, seat, client.clone());
        set_primary_focus(&self.dh, seat, client);
    }

    /// The client under the pointer has said what the pointer should look like.
    ///
    /// Named shapes only, which is what `wp_cursor_shape_manager_v1` gets from a
    /// client and what a browser can draw without being sent a picture: the
    /// names are CSS's names.
    ///
    /// ponytail: a client that sets a cursor surface instead — an older toolkit,
    /// or one drawing a custom cursor — gets the arrow. Streaming that surface
    /// is another window's worth of machinery for a 24-pixel image; do it if a
    /// real application turns out to need it.
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        let name = match image {
            CursorImageStatus::Hidden => "none",
            CursorImageStatus::Named(icon) => icon.name(),
            CursorImageStatus::Surface(_) => "default",
        };
        if self.cursor != name {
            self.cursor = name.to_string();
            self.announce_cursor = true;
        }
    }
}

/// Cursor shapes cover tablet tools as well as pointers, and the protocol's
/// delegate asks for both. There is no tablet here — the browser has a mouse —
/// so the defaults, which do nothing, are the whole implementation.
impl smithay::wayland::tablet_manager::TabletSeatHandler for Webland {}

impl PointerConstraintsHandler for Webland {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        with_pointer_constraint(surface, pointer, |constraint| {
            if let Some(c) = constraint
                && !c.is_active()
            {
                c.activate();
            }
        });
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        self.pointer_location = location;
    }
}

/// Text the clipboard is asked for in, best first. Anything else — an image, a
/// list of files — is a copy the browser has no way to take, and is left alone.
const TEXT_MIMES: [&str; 6] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
    "text/plain;charset=UTF-8",
];

/// The most that will be read out of one copy. A clipboard is not a file
/// transfer, and the whole of it crosses the socket as one message.
const MAX_CLIPBOARD: u64 = 1024 * 1024;

/// The best of what a client is offering, or nothing when none of it is text.
///
/// Order matters: a client that offers both `text/plain` and the utf-8 spelling
/// means the same bytes either way, but one that offers both and means different
/// encodings is answering in whichever was asked for — so ask for the one whose
/// encoding is not a guess.
fn preferred_mime(offered: &[String]) -> Option<&'static str> {
    TEXT_MIMES
        .into_iter()
        .find(|wanted| offered.iter().any(|have| have == wanted))
}

impl SelectionHandler for Webland {
    type SelectionUserData = ();

    /// A client copied something. Read it, so the browser can have it too.
    ///
    /// The client writes into a pipe whenever it gets round to it, so the read
    /// happens on a thread of its own — blocking the compositor on an
    /// application's copy would stop every window on the desktop.
    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        seat: Seat<Self>,
    ) {
        if ty != SelectionTarget::Clipboard && ty != SelectionTarget::Primary {
            return;
        }
        let Some(source) = source else { return };
        let Some(mime) = preferred_mime(&source.mime_types()) else {
            return;
        };
        let Ok((reader, writer)) = std::io::pipe() else {
            return;
        };
        let res = match ty {
            SelectionTarget::Clipboard => {
                request_data_device_client_selection::<Self>(&seat, mime.to_string(), writer.into())
                    .is_ok()
            }
            SelectionTarget::Primary => {
                request_primary_client_selection::<Self>(&seat, mime.to_string(), writer.into())
                    .is_ok()
            }
        };
        if !res {
            return;
        }
        let copied = self.copied.clone();
        std::thread::spawn(move || {
            let mut text = String::new();
            if std::io::Read::read_to_string(
                &mut std::io::Read::take(reader, MAX_CLIPBOARD),
                &mut text,
            )
            .is_ok()
            {
                let _ = copied.send(text);
            }
        });
    }

    /// A client is pasting: hand it whatever the browser last had.
    ///
    /// On a thread for the same reason as the read — a client that asks for the
    /// selection and then does not read the pipe would otherwise block the
    /// compositor once the text outgrew the pipe's buffer.
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        _mime_type: String,
        fd: std::os::fd::OwnedFd,
        _seat: Seat<Self>,
        (): &Self::SelectionUserData,
    ) {
        if ty != SelectionTarget::Clipboard && ty != SelectionTarget::Primary {
            return;
        }
        let text = self.clipboard.clone();
        std::thread::spawn(move || {
            let mut pipe = std::fs::File::from(fd);
            let _ = std::io::Write::write_all(&mut pipe, text.as_bytes());
        });
    }
}

impl DataDeviceHandler for Webland {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for Webland {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl ClientDndGrabHandler for Webland {}
impl ServerDndGrabHandler for Webland {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

/// Per-client state stored behind each `wl_client`.
#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {
        tracing::debug!("client initialized");
    }

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        tracing::debug!("client disconnected");
    }
}

/// The Wayland object behind a surface id the browser named.
///
/// The browser only ever knows [`SurfaceId`]s, which are ours, so every message
/// naming a window arrives needing this translation.
fn object_for(known: &HashMap<ObjectId, Tracked>, id: SurfaceId) -> Option<ObjectId> {
    known
        .iter()
        .find(|(_, tracked)| tracked.id == id)
        .map(|(object, _)| object.clone())
}

/// The toplevel a surface id names, if it is still around.
fn toplevel_for(
    state: &Webland,
    known: &HashMap<ObjectId, Tracked>,
    id: SurfaceId,
) -> Option<ToplevelSurface> {
    let object = object_for(known, id)?;
    state
        .xdg_shell_state
        .toplevel_surfaces()
        .iter()
        .find(|toplevel| toplevel.wl_surface().id() == object)
        .cloned()
}

/// Put an X window at a size, or back to the browser's when there is none.
fn configure_x11(state: &Webland, window: &X11Surface, size: Option<Size>) {
    #[allow(clippy::cast_possible_wrap)]
    let (width, height) = size.map_or(state.size, |size| {
        (size.width.max(1) as i32, size.height.max(1) as i32)
    });
    if let Err(err) = window.configure(Rectangle::from_size((width, height).into())) {
        tracing::warn!(%err, "could not configure an X11 window");
    }
}

/// The X window a surface id names, if it came from X.
fn x11_for_id(
    state: &Webland,
    known: &HashMap<ObjectId, Tracked>,
    id: SurfaceId,
) -> Option<X11Surface> {
    let object = object_for(known, id)?;
    state
        .x11
        .iter()
        .find(|window| window.wl_surface().map(|s| s.id()) == Some(object.clone()))
        .cloned()
}

/// Inject one browser-originated input event into the seat, targeting `surface`.
fn inject_input(
    state: &mut Webland,
    pointer: &PointerHandle<Webland>,
    keyboard: &KeyboardHandle<Webland>,
    surface: &WlSurface,
    event: InputEvent,
    time: u32,
) {
    let serial = SERIAL_COUNTER.next_serial();
    match event {
        InputEvent::PointerMotion { position, .. } => {
            // The browser points at a pixel of the image it was sent; the client
            // is owed a point in its own surface. Those differ by wherever the
            // image was cut from — nothing for a client sent its whole buffer,
            // the shadow margin for one that was cropped to its window, which is
            // a pointer landing a margin's width from where it was pointed.
            let (origin_x, origin_y) = image_origin(surface);
            let location = Point::from((
                position.x + f64::from(origin_x),
                position.y + f64::from(origin_y),
            ));
            let dx = location.x - state.pointer_location.x;
            let dy = location.y - state.pointer_location.y;
            state.pointer_location = location;
            // The single surface sits at the origin: surface-local == compositor.
            pointer.motion(
                state,
                Some((surface.clone(), (0.0, 0.0).into())),
                &MotionEvent {
                    location,
                    serial,
                    time,
                },
            );
            if dx != 0.0 || dy != 0.0 {
                pointer.relative_motion(
                    state,
                    Some((surface.clone(), (0.0, 0.0).into())),
                    &RelativeMotionEvent {
                        delta: (dx, dy).into(),
                        delta_unaccel: (dx, dy).into(),
                        utime: u64::from(time) * 1000,
                    },
                );
            }
            pointer.frame(state);
        }
        InputEvent::PointerMotionRelative { dx, dy } => {
            let is_locked = with_pointer_constraint(surface, pointer, |constraint| {
                constraint
                    .is_some_and(|c| c.is_active() && matches!(*c, PointerConstraint::Locked(_)))
            });
            if is_locked {
                pointer.relative_motion(
                    state,
                    Some((surface.clone(), (0.0, 0.0).into())),
                    &RelativeMotionEvent {
                        delta: (dx, dy).into(),
                        delta_unaccel: (dx, dy).into(),
                        utime: u64::from(time) * 1000,
                    },
                );
            } else {
                let mut next = state.pointer_location;
                next.x += dx;
                next.y += dy;
                state.pointer_location = next;
                pointer.motion(
                    state,
                    Some((surface.clone(), (0.0, 0.0).into())),
                    &MotionEvent {
                        location: next,
                        serial,
                        time,
                    },
                );
                pointer.relative_motion(
                    state,
                    Some((surface.clone(), (0.0, 0.0).into())),
                    &RelativeMotionEvent {
                        delta: (dx, dy).into(),
                        delta_unaccel: (dx, dy).into(),
                        utime: u64::from(time) * 1000,
                    },
                );
            }
            pointer.frame(state);
        }
        InputEvent::PointerButton {
            button,
            state: press,
        } => {
            pointer.button(
                state,
                &ButtonEvent {
                    serial,
                    time,
                    button,
                    state: to_button_state(press),
                },
            );
            pointer.frame(state);
        }
        InputEvent::Key {
            keycode,
            state: press,
        } => {
            // The browser sends evdev codes; xkb keycodes are evdev + 8.
            let code: Keycode = (keycode + 8).into();
            // Only when it actually changes. xkb refcounts a modifier's press,
            // so a second `Down` for a key already held leaves the modifier set
            // after the matching `Up` — and with no pressed key left to show for
            // it, nothing can see it, let alone clear it. The result is a shift
            // or a control that is on for the rest of the compositor's life:
            // letters turn into chords the client answers with a shortcut, and
            // return stops running the command in a terminal.
            //
            // Duplicates are ordinary, not exotic: the browser auto-repeats a
            // held key, and it re-reports a modifier the page missed the release
            // of. Neither is wanted — repeat is the client's own job, from the
            // `wl_keyboard.repeat_info` it was given.
            if keyboard.pressed_keys().contains(&code) == (press == Press::Down) {
                return;
            }
            keyboard.input::<(), _>(state, code, to_key_state(press), serial, time, |_, _, _| {
                FilterResult::Forward
            });
        }
        InputEvent::PointerScroll { dx, dy, .. } => {
            // An axis event has no surface of its own: it goes wherever the
            // pointer's focus is. A wheel turned over a window the pointer has
            // not moved across since it was last raised — parked there, or over
            // a window that just appeared underneath it — would otherwise land
            // in whatever was focused before, so the focus is taken first.
            if pointer.current_focus().as_ref() != Some(surface) {
                pointer.motion(
                    state,
                    Some((surface.clone(), (0.0, 0.0).into())),
                    &MotionEvent {
                        location: state.pointer_location,
                        serial,
                        time,
                    },
                );
            }
            // The browser sends pixels. Clients want both: the continuous value
            // for smooth scrolling, and v120 steps for the ones that only move
            // by whole notches — 120 being one notch, as the wheel protocol has
            // it. Sending neither is why nothing scrolled at all.
            let mut frame = AxisFrame::new(time).source(AxisSource::Wheel);
            for (axis, delta) in [(Axis::Horizontal, dx), (Axis::Vertical, dy)] {
                if delta == 0.0 {
                    continue;
                }
                frame = frame
                    .value(axis, delta)
                    .v120(axis, (delta / WHEEL_NOTCH * 120.0) as i32);
            }
            pointer.axis(state, frame);
            pointer.frame(state);
        }
    }
}

/// Pixels of scroll one wheel notch stands for, which is what a browser reports
/// for one physical click of a mouse wheel.
const WHEEL_NOTCH: f64 = 120.0;

fn to_button_state(press: Press) -> ButtonState {
    match press {
        Press::Down => ButtonState::Pressed,
        Press::Up => ButtonState::Released,
    }
}

fn to_key_state(press: Press) -> KeyState {
    match press {
        Press::Down => KeyState::Pressed,
        Press::Up => KeyState::Released,
    }
}

/// How many frame callbacks may be outstanding before the browser has to catch
/// up. >1 so a client is not stalled by a single round trip.
const INITIAL_FRAME_CREDIT: i32 = 2;

/// Fire callbacks this often even with no credit, so clients still make progress
/// when no browser is attached (otherwise nothing ever renders, nothing is ever
/// sent, and no ack can arrive — a deadlock).
const IDLE_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// The frame interval the pipe is sized against — 60Hz.
const FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

/// The most frames allowed in flight, however long the round trip.
///
/// This is the latency bound from Decision 3 expressed as a number: at 60Hz it
/// is a fifth of a second of queued frames, and a link slower than that should
/// drop frames rather than build a queue nobody wants to watch.
const MAX_FRAME_CREDIT: i32 = 12;

/// Paces `wl_surface.frame` callbacks against the browser (Decision 3).
///
/// Wayland clients redraw only when the compositor fires their frame callback.
/// Firing on the compositor's own loop rate lets a client run ahead of a browser
/// that cannot keep up: it renders into frames the pacer then discards, and
/// latency grows without bound. So a callback costs credit, and credit comes
/// from the browser saying it presented.
struct FrameClock {
    credit: i32,
    /// How many frames may be in flight at once. Adaptive: see [`FrameClock::on_ack`].
    ceiling: i32,
    last_tick: std::time::Instant,
    /// When each unacknowledged frame was sent, oldest first.
    ///
    /// Acks arrive in the order the frames were sent, so pairing them off gives
    /// an exact round trip rather than an estimate.
    sent: std::collections::VecDeque<std::time::Instant>,
    /// Smoothed round trip to the browser and back.
    round_trip: std::time::Duration,
}

impl FrameClock {
    fn new() -> Self {
        Self {
            credit: INITIAL_FRAME_CREDIT,
            ceiling: INITIAL_FRAME_CREDIT,
            last_tick: std::time::Instant::now(),
            sent: std::collections::VecDeque::new(),
            round_trip: std::time::Duration::ZERO,
        }
    }

    /// A frame went out; remember when, so its ack can be timed.
    fn on_send(&mut self, now: std::time::Instant) {
        // Bounded: a browser that stops acking must not grow this without end.
        if self.sent.len() >= MAX_FRAME_CREDIT as usize * 4 {
            self.sent.pop_front();
        }
        self.sent.push_back(now);
    }

    /// The browser presented a frame, so one more redraw is warranted.
    ///
    /// The ceiling on in-flight frames is the round trip divided by the frame
    /// interval — the number of frames that fit in the pipe before the first ack
    /// can possibly return. A fixed ceiling of two is right on loopback and
    /// crippling anywhere else: over a tunnel with an 80ms round trip it caps
    /// the desktop at 25 frames a second however fast the encoder runs, because
    /// the clock cannot advance until an ack completes the trip.
    fn on_ack(&mut self, now: std::time::Instant) {
        if let Some(sent) = self.sent.pop_front() {
            let sample = now.saturating_duration_since(sent);
            // Smoothed, so one slow frame does not move the ceiling far.
            self.round_trip = if self.round_trip.is_zero() {
                sample
            } else {
                (self.round_trip * 3 + sample) / 4
            };
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let fits = (self.round_trip.as_micros() / FRAME_INTERVAL.as_micros()) as i32;
            self.ceiling = fits
                .saturating_add(INITIAL_FRAME_CREDIT)
                .clamp(INITIAL_FRAME_CREDIT, MAX_FRAME_CREDIT);
        }
        self.credit = (self.credit + 1).min(self.ceiling);
    }

    /// Whether to fire frame callbacks this iteration.
    fn should_tick(&mut self, now: std::time::Instant) -> bool {
        if self.credit > 0 {
            self.credit -= 1;
            self.last_tick = now;
            return true;
        }
        // No browser, or one that has gone quiet: keep clients alive slowly.
        if now.duration_since(self.last_tick) >= IDLE_FRAME_INTERVAL {
            self.last_tick = now;
            return true;
        }
        false
    }
}

/// The bounding box of the pixels that actually differ, or `None` if none do.
///
/// Client-declared damage would be cheaper, but it cannot be relied on: a
/// client rendering through Mesa's EGL→`wl_shm` fallback declares the whole
/// surface every frame, which is exactly the case in front of us. Comparing
/// what we captured against what the browser already has costs one pass over
/// the buffer and is true for every client.
fn changed_region(old: &[u8], new: &[u8], size: Size) -> Option<Rect> {
    let stride = size.width as usize * 4;
    if old.len() != new.len() || stride == 0 {
        return None;
    }
    let differs = |y: usize| old[y * stride..(y + 1) * stride] != new[y * stride..(y + 1) * stride];

    let rows = size.height as usize;
    let top = (0..rows).find(|&y| differs(y))?;
    let bottom = (top..rows).rfind(|&y| differs(y))?;
    // Narrow horizontally too: a blinking cursor is one cell, not one line.
    let (mut left, mut right) = (stride, 0);
    for y in top..=bottom {
        let (a, b) = (&old[y * stride..(y + 1) * stride], &new[y * stride..]);
        if let Some(first) = a.iter().zip(b).position(|(x, y)| x != y) {
            left = left.min(first / 4 * 4);
        }
        if let Some(last) = a.iter().zip(b).rposition(|(x, y)| x != y) {
            right = right.max(last / 4 * 4 + 4);
        }
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    Some(Rect {
        x: (left / 4) as i32,
        y: top as i32,
        width: ((right - left) / 4) as u32,
        height: (bottom - top + 1) as u32,
    })
}

/// The size to ask clients to render at, from `WEBLAND_SIZE=WxH` (default 1280x800).
/// The keyboard layout to interpret keys with.
///
/// The browser reports `KeyboardEvent.code`, which is a physical key position
/// and says nothing about what is printed on it: the key labelled A on an AZERTY
/// keyboard reports `KeyQ`. Turning that into a letter is xkb's job, and it can
/// only do it with the right layout — with the default it silently assumes US
/// and every French keyboard types `q` for `a`.
///
/// Taken from the host, because the keyboard is a real one plugged into this
/// machine even though the display is a browser. `WEBLAND_LAYOUT` and
/// `WEBLAND_VARIANT` override it; empty means libxkbcommon's own default.
fn xkb_config() -> XkbConfig<'static> {
    fn leak(value: String) -> &'static str {
        // Leaked deliberately: one small string for the process's lifetime,
        // versus threading a lifetime through the seat for no benefit.
        Box::leak(value.into_boxed_str())
    }

    let layout = std::env::var("WEBLAND_LAYOUT")
        .ok()
        .or_else(host_layout)
        .unwrap_or_default();
    let variant = std::env::var("WEBLAND_VARIANT").unwrap_or_default();
    if !layout.is_empty() {
        tracing::info!(%layout, %variant, "keyboard layout");
    }
    XkbConfig {
        layout: leak(layout),
        variant: leak(variant),
        ..XkbConfig::default()
    }
}

/// The host's configured X11/xkb layout, as systemd records it.
fn host_layout() -> Option<String> {
    let conf = std::fs::read_to_string("/etc/vconsole.conf").ok()?;
    conf.lines()
        .filter_map(|line| line.strip_prefix("XKBLAYOUT="))
        .map(|value| value.trim().trim_matches('"').to_string())
        .find(|value| !value.is_empty())
}

/// The one output clients are told about: the browser's window.
///
/// A compositor with no `wl_output` is legal on the wire and useless in
/// practice. GTK draws anyway, but `WebKit` asks GDK which monitor its window is
/// on before it will composite, gets none, and never paints — a Tauri or GNOME
/// Web window that loads and runs its page into a blank rectangle. The mode is
/// whatever size the browser last reported, so a resize moves the monitor with
/// it.
fn browser_output(dh: &DisplayHandle, size: (i32, i32)) -> Output {
    let output = Output::new(
        String::from("webland"),
        PhysicalProperties {
            // Zero: there is no physical screen, and a made-up millimetre size
            // would only give clients a false DPI to scale by.
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: String::from("Webland"),
            model: String::from("Browser"),
        },
    );
    let _global = output.create_global::<Webland>(dh);
    set_output_mode(&output, size);
    output
}

/// Point the output at a new size, as a monitor changing mode.
fn set_output_mode(output: &Output, (width, height): (i32, i32)) {
    // 60 Hz in millihertz: a number clients divide by, not one anything here
    // paces to — the browser's acks are the real clock.
    let mode = Mode {
        size: (width, height).into(),
        refresh: 60_000,
    };
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        Some(Scale::Integer(1)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
}

fn configured_size() -> (i32, i32) {
    std::env::var("WEBLAND_SIZE")
        .ok()
        .and_then(|value| {
            let (w, h) = value.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        })
        .unwrap_or((1280, 800))
}

/// Bring up a GLES renderer on the render node, for clients that hand us GPU
/// buffers instead of shared memory.
///
/// Returns the renderer and the node's device id, or `None` — with a warning,
/// not an error — if there is no usable render node: `wl_shm` clients still work
/// without one, they are just the slow path. `--example gpu_probe` is the quick
/// way to find out why this failed.
fn open_gpu() -> Option<(GlesRenderer, u64)> {
    let path = render_node();
    let open = || -> Result<(GlesRenderer, u64), Box<dyn std::error::Error>> {
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .open(&path)?;
        // Read before the file moves into gbm: dmabuf feedback names the device
        // by its dev_t, and that is how Mesa knows which node to open.
        let device = std::os::unix::fs::MetadataExt::rdev(&file.metadata()?);
        let gbm = GbmDevice::new(file)?;
        // SAFETY: the display owns the gbm device for the rest of the process,
        // and we hand its fd to nothing else.
        #[allow(unsafe_code)]
        let egl = unsafe { EGLDisplay::new(gbm) }?;
        let context = EGLContext::new(&egl)?;
        // SAFETY: called once, on the thread that owns the context, and this
        // renderer never leaves that thread.
        #[allow(unsafe_code)]
        let renderer = unsafe { GlesRenderer::new(context) }?;
        Ok((renderer, device))
    };
    match open() {
        Ok(gpu) => {
            tracing::info!(node = %path, "GPU up; offering linux-dmabuf-v1");
            Some(gpu)
        }
        Err(err) => {
            tracing::warn!(node = %path, %err, "no GPU; clients fall back to wl_shm");
            None
        }
    }
}

/// What a client's dmabuf looks like to the encoder: fds and their layout, and
/// nothing that requires reading a single pixel.
struct Planes {
    size: Size,
    fourcc: u32,
    modifier: u64,
    /// `(fd, offset, stride)`, one per plane.
    layout: Vec<(i32, u32, u32)>,
}

/// A window's current title, wherever it keeps one.
///
/// An X client sets `WM_NAME` on its X window rather than through `xdg_shell`,
/// so the two have to be asked separately for the same answer.
/// Which application a surface belongs to.
///
/// `app_id` for a Wayland client and `WM_CLASS` for an X11 one: both are meant
/// to name the `.desktop` file the window came from, which is what lets the
/// panel find its icon.
fn surface_app_id(state: &Webland, surface: &WlSurface) -> Option<String> {
    if let Some(window) = state.x11_for(surface) {
        let class = window.class();
        return (!class.is_empty()).then_some(class);
    }
    smithay::wayland::compositor::with_states(surface, |states| {
        states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok()?.app_id.clone())
    })
}

fn surface_title(state: &Webland, surface: &WlSurface) -> Option<String> {
    if let Some(window) = state.x11_for(surface) {
        let title = window.title();
        return (!title.is_empty()).then_some(title);
    }
    toplevel_title(surface)
}

/// A toplevel's current title, as the client last set it.
fn toplevel_title(surface: &WlSurface) -> Option<String> {
    smithay::wayland::compositor::with_states(surface, |states| {
        states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok()?.title.clone())
    })
}

/// Describe a surface's committed dmabuf, if it committed one.
fn dmabuf_planes(surface: &WlSurface) -> Option<Planes> {
    // Only a client that draws its whole window into one buffer can hand that
    // buffer straight to the encoder. One with subsurfaces has to be composited
    // first, and `capture` does that.
    if !get_children(surface).is_empty() {
        return None;
    }
    let buffer = with_renderer_surface_state(surface, |s| s.buffer().cloned())??;
    let dmabuf = get_dmabuf(&buffer).ok()?;
    let format = dmabuf.format();
    let planes = dmabuf
        .handles()
        .map(|fd| std::os::fd::AsRawFd::as_raw_fd(&fd))
        .zip(dmabuf.offsets())
        .zip(dmabuf.strides())
        .map(|((fd, offset), stride)| (fd, offset, stride))
        .collect();
    Some(Planes {
        size: Size {
            width: dmabuf.width(),
            height: dmabuf.height(),
        },
        fourcc: format.code as u32,
        modifier: u64::from(format.modifier),
        layout: planes,
    })
}

/// Copy a surface's committed contents, tightly packed, whichever kind of buffer
/// the client committed.
fn capture(renderer: Option<&mut GlesRenderer>, surface: &WlSurface) -> Option<(Size, Vec<u8>)> {
    // A client with subsurfaces has to be composited, which needs a renderer —
    // so under winit, which has none to spare here, such a window sends nothing.
    if !get_children(surface).is_empty() {
        return capture_tree(renderer?, surface);
    }
    match capture_shm(surface) {
        Some(captured) => Some(captured),
        None => capture_dmabuf(renderer?, surface),
    }
}

/// The window's own rectangle within its surface tree, which a decorated client
/// sets to exclude the shadow it draws around itself.
///
/// Falls back to the toplevel's whole buffer for a client that sets no geometry:
/// that brings the shadow along, which still beats a black window.
fn window_geometry(surface: &WlSurface) -> Option<Rectangle<i32, Logical>> {
    let geometry = smithay::wayland::compositor::with_states(surface, |states| {
        states
            .cached_state
            .get::<SurfaceCachedState>()
            .current()
            .geometry
    });
    if let Some(geometry) = geometry {
        return Some(geometry);
    }
    let size = with_renderer_surface_state(surface, |s| s.buffer_size())??;
    Some(Rectangle::from_size((size.w, size.h).into()))
}

/// Where a surface hangs, if it is a popup rather than a window.
///
/// The offset is the popup's own geometry, which the positioner already
/// expressed relative to the parent's window geometry — the same rectangle the
/// browser clips the parent to, so the two agree about where the corner is.
fn popup_anchor(
    state: &Webland,
    known: &HashMap<ObjectId, Tracked>,
    surface: &WlSurface,
) -> Option<Anchor> {
    let popup = state
        .popups
        .iter()
        .find(|popup| popup.wl_surface().id() == surface.id())?;
    let parent = known.get(&popup.get_parent_surface()?.id())?.id;
    let geometry = popup.with_pending_state(|state| state.geometry);
    Some(Anchor {
        parent,
        x: geometry.loc.x,
        y: geometry.loc.y,
    })
}

/// Close every popup that the newly focused surface does not belong to.
///
/// This is what a pointer grab would do in a compositor that took one: a menu
/// stays up until something outside it is clicked, and then it goes. The chain
/// matters — clicking a submenu must not close the menu it opened from — so
/// what survives is the focused surface and its ancestors.
fn dismiss_popups(state: &mut Webland, focused: Option<ObjectId>) {
    let keep = ancestry(focused, |id| {
        state
            .popups
            .iter()
            .find(|popup| popup.wl_surface().id() == *id)
            .and_then(PopupSurface::get_parent_surface)
            .map(|parent| parent.id())
    });
    state.popups.retain(|popup| {
        if keep.contains(&popup.wl_surface().id()) {
            return true;
        }
        popup.send_popup_done();
        false
    });
}

/// A surface and everything it hangs from, nearest first.
///
/// The chain is the client's to describe, so it is not to be trusted with a
/// loop: a popup that claims to be its own ancestor would spin here forever,
/// taking the compositor with it.
fn ancestry<T: Clone + PartialEq>(start: Option<T>, parent_of: impl Fn(&T) -> Option<T>) -> Vec<T> {
    let mut chain: Vec<T> = Vec::new();
    let mut current = start;
    while let Some(id) = current {
        if chain.contains(&id) {
            break;
        }
        current = parent_of(&id);
        chain.push(id);
    }
    chain
}

/// Where the streamed image's top-left sits in the surface's own coordinates.
///
/// The two capture paths disagree, and everything that maps between browser
/// pixels and client pixels has to know which one ran: a surface with
/// subsurfaces is composited by [`capture_tree`], which crops to the window
/// rectangle, while one that draws into a single buffer is sent whole, shadow
/// margin and all. Same rule as [`capture`] picks by, in one place so a pointer
/// cannot land somewhere the picture never showed.
fn image_origin(surface: &WlSurface) -> (i32, i32) {
    if get_children(surface).is_empty() {
        return (0, 0);
    }
    window_geometry(surface).map_or((0, 0), |geometry| (geometry.loc.x, geometry.loc.y))
}

/// The window itself within the streamed image: what the browser should show,
/// and nothing the client drew around it.
///
/// A client's shadow margin is transparent to the client and black once
/// encoded, so a browser that drew the whole image would frame every window in
/// black. Clamped to the image, because a client is free to declare a geometry
/// larger than what it committed.
fn content_rect(surface: &WlSurface, image: Size) -> Rect {
    let Some(geometry) = window_geometry(surface) else {
        return Rect {
            x: 0,
            y: 0,
            width: image.width,
            height: image.height,
        };
    };
    let origin = image_origin(surface);
    clip(
        (geometry.loc.x - origin.0, geometry.loc.y - origin.1),
        (geometry.size.w, geometry.size.h),
        image,
    )
}

/// The window rectangle as the browser can actually use it: inside the image,
/// never empty, never negative.
fn clip(offset: (i32, i32), size: (i32, i32), image: Size) -> Rect {
    #[allow(clippy::cast_sign_loss)]
    let (x, y) = (
        offset.0.max(0).unsigned_abs().min(image.width),
        offset.1.max(0).unsigned_abs().min(image.height),
    );
    #[allow(clippy::cast_sign_loss)]
    let (width, height) = (size.0.max(0).unsigned_abs(), size.1.max(0).unsigned_abs());
    Rect {
        x: i32::try_from(x).unwrap_or(0),
        y: i32::try_from(y).unwrap_or(0),
        width: width.min(image.width - x).max(1),
        height: height.min(image.height - y).max(1),
    }
}

/// Composite a surface and its subsurfaces into one image.
///
/// A client with client-side decorations — every GTK app, Firefox among them —
/// commits only the shadow frame to its toplevel and puts the window's actual
/// contents in a subsurface. Reading the toplevel's own buffer therefore gives
/// a black window, so render the whole tree and read that back instead.
///
/// ponytail: a texture allocated per frame, then read back to the CPU — which
/// costs such a client the zero-copy path, landing it on the CPU encoder like an
/// shm client. Rendering into one gbm-allocated dmabuf held across frames and
/// handing the encoder its fds is the upgrade.
fn capture_tree(renderer: &mut GlesRenderer, surface: &WlSurface) -> Option<(Size, Vec<u8>)> {
    let geometry = window_geometry(surface)?;
    let (width, height) = (geometry.size.w, geometry.size.h);
    if width <= 0 || height <= 0 {
        return None;
    }

    let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        render_elements_from_surface_tree(
            renderer,
            surface,
            (-geometry.loc.x, -geometry.loc.y),
            1.0,
            1.0,
            Kind::Unspecified,
        );

    let mut texture: GlesTexture = renderer
        .create_buffer(Fourcc::Argb8888, (width, height).into())
        .ok()?;
    // Scoped so the framebuffer releases the texture before it is read back.
    {
        let damage = [Rectangle::from_size((width, height).into())];
        let mut framebuffer = renderer.bind(&mut texture).ok()?;
        let mut frame = renderer
            .render(&mut framebuffer, (width, height).into(), Transform::Normal)
            .ok()?;
        frame
            .clear(Color32F::new(0.0, 0.0, 0.0, 0.0), &damage)
            .ok()?;
        draw_render_elements(&mut frame, 1.0, &elements, &damage).ok()?;
        // Waited on, not dropped: the readback below must see finished pixels.
        frame.finish().ok()?.wait().ok()?;
    }
    let mapping = renderer
        .copy_texture(
            &texture,
            Rectangle::from_size((width, height).into()),
            Fourcc::Argb8888,
        )
        .ok()?;
    let size = Size {
        width: width as u32,
        height: height as u32,
    };
    Some((size, renderer.map_texture(&mapping).ok()?.to_vec()))
}

/// Every commit counter in a surface tree.
///
/// A desynchronised subsurface — which is how a decorated client draws its next
/// frame — commits without touching the toplevel, so watching the toplevel's own
/// counter would freeze the window on whatever it showed first.
fn tree_commits(root: &WlSurface) -> Vec<CommitCounter> {
    let mut commits = Vec::new();
    with_surface_tree_downward(
        root,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_, states, &()| {
            // Read the state here rather than through `with_renderer_surface_state`:
            // that re-enters the lock this traversal already holds, and deadlocks.
            if let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>()
                && let Ok(state) = data.lock()
            {
                commits.push(state.current_commit());
            }
        },
        |_, _, &()| true,
    );
    commits
}

/// Copy a surface's committed dmabuf contents by way of the GPU.
///
/// ponytail: this reads the buffer back to the CPU, which is precisely what
/// Decision 2 forbids — the frame then goes down the same Deflate path as an
/// shm one. It exists so that advertising the dmabuf global does not black out
/// every client that takes it up. Deleted when VA-API encodes from the dmabuf
/// directly, which is the actual gate.
fn capture_dmabuf(renderer: &mut GlesRenderer, surface: &WlSurface) -> Option<(Size, Vec<u8>)> {
    let buffer = with_renderer_surface_state(surface, |s| s.buffer().cloned())??;
    let dmabuf = get_dmabuf(&buffer).ok()?;
    let size = Size {
        width: dmabuf.width(),
        height: dmabuf.height(),
    };
    #[allow(clippy::cast_possible_wrap)]
    let region = Rectangle::from_size((size.width as i32, size.height as i32).into());
    let texture = renderer.import_dmabuf(dmabuf, None).ok()?;
    let mapping = renderer
        .copy_texture(&texture, region, Fourcc::Argb8888)
        .ok()?;
    Some((size, renderer.map_texture(&mapping).ok()?.to_vec()))
}

/// Copy a surface's committed `wl_shm` contents, tightly packed.
///
/// Returns `None` if the surface has no buffer, or a buffer that is not shm —
/// `capture_dmabuf` handles the latter.
fn capture_shm(surface: &WlSurface) -> Option<(Size, Vec<u8>)> {
    with_renderer_surface_state(surface, |renderer_state| {
        let buffer = renderer_state.buffer()?;
        with_buffer_contents(buffer, |ptr, len, data| {
            #[allow(clippy::cast_sign_loss)]
            let size = Size {
                width: data.width.max(0) as u32,
                height: data.height.max(0) as u32,
            };
            let whole = Rect {
                x: 0,
                y: 0,
                width: size.width,
                height: size.height,
            };
            // SAFETY: smithay guarantees `ptr` addresses `len` initialized bytes
            // of the shm pool mapping for the duration of this callback.
            #[allow(unsafe_code)]
            let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
            (size, crop(bytes, data.offset, data.stride, whole))
        })
        .ok()
    })
    .flatten()
}

/// Pack `region` out of a strided BGRA buffer into tightly-packed rows.
///
/// Returns empty if the buffer is shorter than the region implies, rather than
/// reading past it — the pool is client-controlled memory.
#[allow(clippy::cast_sign_loss)]
fn crop(bytes: &[u8], offset: i32, stride: i32, region: Rect) -> Vec<u8> {
    let (offset, stride) = (offset.max(0) as usize, stride.max(0) as usize);
    let row_bytes = region.width as usize * 4;
    let mut pixels = Vec::with_capacity(row_bytes * region.height as usize);
    for row in 0..region.height as usize {
        let start = offset + (region.y as usize + row) * stride + region.x as usize * 4;
        let Some(source) = bytes.get(start..start + row_bytes) else {
            return Vec::new();
        };
        pixels.extend_from_slice(source);
    }
    pixels
}

/// Fire the frame callbacks on a surface tree so clients render their next frame.
fn send_frames_surface_tree(surface: &wl_surface::WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

/// Drain browser messages: inject input into the seat, and credit the frame
/// clock for every frame the browser reports presented.
/// What `drain_client` needs that does not change from one pass to the next.
struct Session<'a> {
    keyboard: &'a KeyboardHandle<Webland>,
    pointer: &'a PointerHandle<Webland>,
    start_time: std::time::Instant,
    applications: &'a apps::Applications,
    env: &'a spawn::Env,
}

fn drain_client(
    state: &mut Webland,
    poll_client: &mut Option<Box<dyn FnMut() -> Option<ClientMessage>>>,
    known: &mut HashMap<ObjectId, Tracked>,
    session: &Session<'_>,
) {
    let Session {
        keyboard,
        pointer,
        start_time,
        applications,
        env,
    } = session;
    let start_time = *start_time;
    let Some(poll) = poll_client.as_mut() else {
        return;
    };
    let mut events = Vec::new();
    let mut focus_changed = false;
    let mut resize = None;
    let mut closing = Vec::new();
    let mut launching = Vec::new();
    let mut maximizing = Vec::new();
    let mut sizing = Vec::new();
    while let Some(message) = poll() {
        match message {
            ClientMessage::Input(event) => events.push(event),
            ClientMessage::FramePresented { id } => {
                if let Some(tracked) = known.values_mut().find(|tracked| tracked.id == id) {
                    tracked.clock.on_ack(std::time::Instant::now());
                }
            }
            ClientMessage::RequestKeyframe => {
                state.keyframe = true;
                // "Send me everything" includes what the launcher can start: a
                // browser that just connected has no list yet.
                state.announce_applications = true;
                // And what the pointer looks like: the browser's own default is
                // an arrow, whatever the client last asked for.
                state.announce_cursor = true;
            }
            ClientMessage::Clipboard { text } => {
                // Handled here, in the drain, rather than with the input below:
                // the paste keystroke is in this same batch and is injected
                // after it, so the client asks for the selection only once the
                // selection is the one the user meant.
                state.clipboard = text;
                set_data_device_selection(
                    &state.dh.clone(),
                    &state.seat.clone(),
                    TEXT_MIMES.iter().map(|mime| (*mime).to_string()).collect(),
                    (),
                );
                set_primary_selection(
                    &state.dh.clone(),
                    &state.seat.clone(),
                    TEXT_MIMES.iter().map(|mime| (*mime).to_string()).collect(),
                    (),
                );
            }
            ClientMessage::Focus { id } => {
                state.focus = Some(id);
                focus_changed = true;
            }
            ClientMessage::Resize { size } => resize = Some(size),
            ClientMessage::CloseSurface { id } => closing.push(id),
            ClientMessage::Launch { id } => launching.push(id),
            ClientMessage::SetMaximized { id, size } => maximizing.push((id, size)),
            ClientMessage::SetSize { id, size } => sizing.push((id, size)),
            // The tray lives on the session bus and is the server's business;
            // these are answered before they ever reach the compositor.
            ClientMessage::TrayActivate { .. }
            | ClientMessage::TrayMenuOpen { .. }
            | ClientMessage::TrayMenuClick { .. } => {}
        }
    }
    if focus_changed {
        let focused = state.focus.and_then(|id| object_for(known, id));
        dismiss_popups(state, focused.clone());
        state.activate_x11(focused.as_ref());
    }

    for id in launching {
        applications.launch(id, env);
    }

    for id in closing {
        if let Some(toplevel) = toplevel_for(state, known, id) {
            toplevel.send_close();
        } else if let Some(window) = x11_for_id(state, known, id)
            && let Err(err) = window.close()
        {
            tracing::warn!(%err, "could not ask an X11 window to close");
        }
    }

    // Maximizing is the client's job: it is told the size and the state, and
    // redraws itself to fit. The shell only moves the window to the corner.
    for (id, size) in maximizing {
        let Some(toplevel) = toplevel_for(state, known, id) else {
            // An X window is told a size and nothing else. `set_maximized`
            // exists, but it sets a hint the client reads back — the size is
            // what actually makes it redraw, and restoring means the size the
            // browser is showing rather than one the client remembers.
            if let Some(window) = x11_for_id(state, known, id) {
                configure_x11(state, &window, size);
            }
            continue;
        };
        toplevel.with_pending_state(|pending| {
            // `None` leaves the size to the client, which is how it gets back
            // to whatever it was before being maximized.
            #[allow(clippy::cast_possible_wrap)]
            {
                pending.size = size.map(|s| (s.width as i32, s.height as i32).into());
            }
            if size.is_some() {
                pending.states.set(xdg_toplevel::State::Maximized);
            } else {
                pending.states.unset(xdg_toplevel::State::Maximized);
            }
        });
        toplevel.send_configure();
    }

    // A resize grip is the same configure, minus the state: the client is told
    // a size and redraws at it. Maximized comes off, because a window the user
    // has just dragged to a size of their own is not maximized any more — and a
    // client left flagged maximized would keep drawing as if it were.
    for (id, size) in sizing {
        let Some(toplevel) = toplevel_for(state, known, id) else {
            if let Some(window) = x11_for_id(state, known, id) {
                configure_x11(state, &window, Some(size));
            }
            continue;
        };
        toplevel.with_pending_state(|pending| {
            #[allow(clippy::cast_possible_wrap)]
            {
                pending.size = Some((size.width.max(1) as i32, size.height.max(1) as i32).into());
            }
            pending.states.unset(xdg_toplevel::State::Maximized);
        });
        toplevel.send_configure();
    }

    // Reconfigure every toplevel when the browser's window changes size. The
    // client redraws at the new size and the next capture picks it up, which is
    // what turns a browser resize into a sharp surface rather than a scaled one.
    if let Some(size) = resize {
        #[allow(clippy::cast_possible_wrap)]
        let wanted = (size.width.max(1) as i32, size.height.max(1) as i32);
        if state.size != wanted {
            state.size = wanted;
            set_output_mode(&state.output, wanted);
            state.resize_x11();
            for toplevel in state.xdg_shell_state.toplevel_surfaces() {
                toplevel.with_pending_state(|pending| {
                    pending.size = Some(wanted.into());
                });
                toplevel.send_configure();
            }
        }
    }

    // Input goes to the surface the browser raised; before it has raised
    // anything, to whichever surface exists.
    //
    // Over everything that is streamed, not over the toplevels: a popup and an
    // X window are both surfaces the browser shows and the user clicks, and
    // looking only where `xdg_shell` keeps its windows meant an X client was
    // sent every frame and handed no click or keystroke back — Steam, and
    // everything else that never grew a Wayland backend, drawn but dead.
    let focused = state.focus.and_then(|id| object_for(known, id));
    let surfaces = streamed_surfaces(state);
    let target = surfaces
        .iter()
        .find(|surface| Some(surface.id()) == focused)
        .or_else(|| surfaces.first())
        .cloned();
    if state.focus.is_none()
        && let Some(surface) = &target
        && let Some(tracked) = known.get(&surface.id())
    {
        state.focus = Some(tracked.id);
        state.activate_x11(Some(&surface.id()));
    }
    if events.is_empty() {
        return;
    }
    let now = start_time.elapsed().as_millis() as u32;
    // The keyboard follows the focus, and only when it actually changes:
    // re-focusing what is already focused makes smithay resend `enter` and
    // `modifiers` for nothing.
    if let Some(surface) = &target
        && keyboard.current_focus().as_ref() != Some(surface)
    {
        keyboard.set_focus(state, Some(surface.clone()), SERIAL_COUNTER.next_serial());
    }
    for event in events {
        // The pointer follows the cursor. A motion or a wheel names the
        // surface the browser delivered it to, which is the window under the
        // cursor and not necessarily the focused one — sending it to the focus
        // instead meant an unfocused window never saw `pointer.enter`, never
        // highlighted anything under the cursor and never scrolled, while the
        // focused one was silently pointed at coordinates from another window.
        let surface = match pointer_surface(&event) {
            Some(id) => {
                let object = object_for(known, id);
                surfaces
                    .iter()
                    .find(|surface| Some(surface.id()) == object)
                    .cloned()
            }
            None => target.clone(),
        };
        // A surface the browser still holds and the compositor has already
        // dropped: skip it rather than deliver it somewhere it was not aimed.
        if let Some(surface) = surface {
            inject_input(state, pointer, keyboard, &surface, event, now);
        }
    }
}

/// The surface a pointer event was delivered to, for the events that name one.
///
/// Buttons are absent on purpose: a `wl_pointer.button` has no surface of its
/// own and goes to whatever the pointer's focus is, which the motion that put
/// the cursor there has already set.
fn pointer_surface(event: &InputEvent) -> Option<SurfaceId> {
    match *event {
        InputEvent::PointerMotion { id, .. } | InputEvent::PointerScroll { id, .. } => Some(id),
        _ => None,
    }
}

/// What the browser has been told about one surface.
struct Tracked {
    id: SurfaceId,
    size: Option<Size>,
    /// The commits the browser's pixels came from — one per surface in the
    /// tree — so an untouched window costs nothing to skip.
    commits: Vec<CommitCounter>,
    /// The pixels the browser is holding, to diff the next capture against.
    /// Only the deflate path needs these; H.264 keeps its own reference frames.
    pixels: Vec<u8>,
    /// Built on first use and thrown away on resize, since both the filter graph
    /// and the encoder bake the dimensions in.
    encoder: Option<encode::Encoder>,
    /// This surface's own pacing. One clock for the whole desktop would let a
    /// busy window spend the frame callbacks owed to the quiet ones.
    clock: FrameClock,
    /// The title the browser has been told, so an unchanged one costs nothing.
    title: Option<String>,
    /// The application id the browser has been told, likewise.
    app_id: Option<String>,
    /// Where a popup was last said to hang. A menu the client repositions keeps
    /// its size, so a size change alone would not notice one moving.
    anchor: Option<Anchor>,
    /// Whether the surface has an active pointer lock constraint.
    locked: bool,
}

/// Capture changed surfaces and emit their frames to the browser transport.
///
/// Only pixels that actually changed go on the wire, so an idle surface costs
/// nothing at all. A browser joining mid-stream has nothing for a damage
/// rectangle to land on, so it asks for a keyframe and gets whole surfaces once.
fn stream_dirty(
    state: &mut Webland,
    applications: &apps::Applications,
    mut renderer: Option<&mut GlesRenderer>,
    on_frame: Option<&dyn Fn(ServerMessage)>,
    known: &mut HashMap<ObjectId, Tracked>,
    next_surface_id: &mut u64,
) {
    let Some(emit) = on_frame else {
        return;
    };
    let keyframe = std::mem::take(&mut state.keyframe);
    if std::mem::take(&mut state.announce_applications) {
        emit(ServerMessage::Applications(applications.listing()));
    }
    // Anything a client copied since the last pass. The browser puts it on the
    // real clipboard, which is what makes a copy here paste anywhere else.
    while let Ok(text) = state.pastes.try_recv() {
        state.clipboard.clone_from(&text);
        emit(ServerMessage::Clipboard { text });
    }
    if std::mem::take(&mut state.announce_cursor) {
        emit(ServerMessage::Cursor {
            name: state.cursor.clone(),
        });
    }
    // A request names a surface the browser has never heard of until that
    // surface has been announced; one arriving that early is dropped rather than
    // queued, since the gesture it belongs to is over by the time a window
    // exists to apply it to.
    for (surface, request) in std::mem::take(&mut state.requests) {
        if let Some(tracked) = known.get(&surface) {
            emit(ServerMessage::SurfaceRequest {
                id: tracked.id,
                request,
            });
        }
    }
    // Popups are surfaces like any other: they are captured, encoded and sent
    // down the same path a window is. What makes one a popup is where the
    // browser puts it, which is the anchor announced with it.
    state.popups.retain(PopupSurface::alive);
    let focused = state.focus.and_then(|id| object_for(known, id));
    let toplevels = streamed_surfaces(state);
    for surface in &toplevels {
        // Read before the surface is tracked, because both want `known` and the
        // parent's id has to be in it already — it is, since a popup cannot be
        // mapped before the surface it hangs from.
        let anchor = popup_anchor(state, known, surface);
        let mut is_new = false;
        let tracked = known.entry(surface.id()).or_insert_with(|| {
            is_new = true;
            let id = SurfaceId(*next_surface_id);
            *next_surface_id += 1;
            Tracked {
                id,
                size: None,
                commits: Vec::new(),
                pixels: Vec::new(),
                encoder: None,
                clock: FrameClock::new(),
                title: None,
                app_id: None,
                anchor: None,
                locked: false,
            }
        });
        if is_new && anchor.is_none() {
            state.focus = Some(tracked.id);
            state.activate_x11(Some(&surface.id()));
        }

        if let Some(pointer) = state.seat.get_pointer() {
            if focused.as_ref() == Some(&surface.id()) {
                with_pointer_constraint(surface, &pointer, |constraint| {
                    if let Some(c) = constraint
                        && !c.is_active()
                    {
                        c.activate();
                    }
                });
            } else {
                with_pointer_constraint(surface, &pointer, |constraint| {
                    if let Some(c) = constraint
                        && c.is_active()
                    {
                        c.deactivate();
                    }
                });
            }
        }
        let is_locked = state.seat.get_pointer().is_some_and(|pointer| {
            with_pointer_constraint(surface, &pointer, |constraint| {
                constraint
                    .is_some_and(|c| c.is_active() && matches!(*c, PointerConstraint::Locked(_)))
            })
        });
        if tracked.locked != is_locked {
            tracked.locked = is_locked;
            emit(ServerMessage::PointerConstraint {
                id: tracked.id,
                locked: is_locked,
            });
        }

        let commits = tree_commits(surface);
        if commits.is_empty() || (tracked.commits == commits && !keyframe) {
            continue;
        }
        // Prefer the client's own GPU buffer: its dimensions are known without
        // reading it, so an encodable surface never gets copied at all.
        let dmabuf = dmabuf_planes(surface);
        let mut pixels = None;
        let size = match &dmabuf {
            Some(planes) => planes.size,
            None => match capture(renderer.as_deref_mut(), surface) {
                Some((size, captured)) if !captured.is_empty() => {
                    pixels = Some(captured);
                    size
                }
                _ => continue,
            },
        };
        tracked.commits = commits;

        // Asked every announce rather than once: a client creates its decoration
        // object early, but nothing in the protocol makes it do so before its
        // first buffer, and a resize or a keyframe re-sends this anyway.
        let decorates_itself = state.decorates_itself(surface);

        // A resize invalidates whatever the browser is holding, and so does a
        // browser that has just joined: both take the whole surface.
        let resized = tracked.size != Some(size);
        let moved = tracked.anchor != anchor;
        tracked.anchor = anchor;
        let announced = keyframe || resized || moved;
        if announced {
            tracked.size = Some(size);
            // Temporary: the streamed image against the window the client says
            // it drew. A wider image is the client's shadow margin, which is
            // transparent to the client and black once encoded.
            tracing::debug!(
                id = tracked.id.0,
                image = ?(size.width, size.height),
                content = ?content_rect(surface, size),
                decorated = !decorates_itself,
                "surface announced"
            );
            emit(ServerMessage::SurfaceCreated(SurfaceCreated {
                id: tracked.id,
                size,
                content: content_rect(surface, size),
                parent: anchor,
                decorated: !decorates_itself,
            }));
        }
        // Only a resize needs a new encoder — the filter graph and the codec
        // both bake the dimensions in. A keyframe request does not: `announced`
        // is passed to the encoder below and forces an IDR on the stream it
        // already has. Rebuilding one per request meant a fresh VA-API context
        // on every page load, and once those ran out encoding stopped dead.
        if resized {
            tracked.encoder = None;
        }

        // After `SurfaceCreated`, never before: the browser hangs a title on a
        // window it already knows about, and one for a surface it has not been
        // told about yet is dropped. Cleared on announce for the same reason a
        // keyframe resends pixels — a browser that just arrived has heard
        // nothing, whatever the last one was told.
        if announced {
            tracked.title = None;
            tracked.app_id = None;
        }
        let app_id = surface_app_id(state, surface);
        if app_id.is_some() && tracked.app_id != app_id {
            tracked.app_id.clone_from(&app_id);
            if let Some(app_id) = app_id {
                emit(ServerMessage::SurfaceAppId {
                    id: tracked.id,
                    app_id,
                });
            }
        }
        let title = surface_title(state, surface);
        if title.is_some() && tracked.title != title {
            tracked.title.clone_from(&title);
            if let Some(title) = title {
                emit(ServerMessage::SurfaceTitle {
                    id: tracked.id,
                    title,
                });
            }
        }

        // H.264 first: it is the only codec that gets a scrolling terminal into
        // a sane bitrate (gate 4). Damage is empty on this path — the encoder
        // decides for itself what changed, and says so far better than a
        // bounding box can.
        let wanted = if dmabuf.is_some() {
            encode::Input::Dmabuf
        } else {
            encode::Input::Cpu
        };
        // A client that switches buffer kinds needs a different graph entirely.
        if tracked
            .encoder
            .as_ref()
            .is_some_and(|e| e.input() != wanted)
        {
            tracked.encoder = None;
        }
        if tracked.encoder.is_none() {
            let (fourcc, modifier) = dmabuf
                .as_ref()
                .map_or((0, 0), |planes| (planes.fourcc, planes.modifier));
            match encode::Encoder::new(
                &render_node(),
                size.width,
                size.height,
                bitrate(),
                wanted,
                fourcc,
                modifier,
            ) {
                Ok(encoder) => tracked.encoder = Some(encoder),
                Err(err) => tracing::debug!(%err, "no H.264 encoder; sending deflate"),
            }
        }
        if let Some(encoder) = tracked.encoder.as_mut() {
            let encoded = match (&dmabuf, pixels.as_ref()) {
                (Some(p), _) => encoder.encode_dmabuf(&p.layout, p.fourcc, p.modifier, announced),
                (None, Some(bgra)) => encoder.encode(bgra, announced),
                (None, None) => encode::Encoded::Failed,
            };
            match encoded {
                encode::Encoded::Packet(payload) => {
                    emit(ServerMessage::SurfaceFrame(SurfaceFrame {
                        id: tracked.id,
                        codec: Codec::H264,
                        damage: Vec::new(),
                        payload,
                    }));
                    tracked.clock.on_send(std::time::Instant::now());
                    continue;
                }
                // The encoder has the frame and will emit it with the next one.
                // Sending the same picture by another codec would duplicate it
                // and leave the video stream missing what it already consumed.
                encode::Encoded::Pending => continue,
                encode::Encoded::Failed => {}
            }
        }

        // The encoder could not take this frame. Fall back to reading the buffer
        // back and deflating what changed, which works for anything.
        let Some(pixels) = pixels
            .or_else(|| capture(renderer.as_deref_mut(), surface).map(|(_, captured)| captured))
        else {
            continue;
        };
        if pixels.is_empty() {
            continue;
        }
        let whole = Rect {
            x: 0,
            y: 0,
            width: size.width,
            height: size.height,
        };
        let region = if announced {
            whole
        } else {
            match changed_region(&tracked.pixels, &pixels, size) {
                Some(region) => region,
                // Committed, but the pixels are identical: nothing to send.
                None => continue,
            }
        };

        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let payload = crop(&pixels, 0, (size.width * 4) as i32, region);
        tracked.pixels = pixels;
        emit(ServerMessage::SurfaceFrame(SurfaceFrame {
            id: tracked.id,
            codec: Codec::Deflate,
            damage: vec![region],
            payload: webland_protocol::deflate(&payload),
        }));
        tracked.clock.on_send(std::time::Instant::now());
    }

    // Tell the browser about anything that has gone. A closed window would
    // otherwise sit on screen for good: from the far end an idle surface and a
    // dead one look identical, both being simply an absence of frames.
    let live: Vec<ObjectId> = toplevels.iter().map(Resource::id).collect();
    known.retain(|id, tracked| {
        let alive = live.contains(id);
        if !alive {
            emit(ServerMessage::SurfaceDestroyed { id: tracked.id });
            if state.focus == Some(tracked.id) {
                state.focus = None;
            }
        }
        alive
    });
    state.server_decorated.retain(|id| live.contains(id));
}

/// The render node to open, for both the dmabuf global and the encoder.
fn render_node() -> String {
    std::env::var("WEBLAND_RENDER_NODE").unwrap_or_else(|_| String::from("/dev/dri/renderD128"))
}

/// Encoder target bitrate. A desktop is mostly still, so the encoder spends far
/// less than this in practice; it is a ceiling for the worst case.
fn bitrate() -> i64 {
    std::env::var("WEBLAND_BITRATE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8_000_000)
}

/// Every surface the browser is shown: windows, the menus they open, and the
/// windows that came from X.
///
/// One list, because two things walk it — the capture that sends pixels and the
/// frame clock that asks for the next ones — and a surface in one but not the
/// other is a window that either freezes or is never drawn.
fn streamed_surfaces(state: &Webland) -> Vec<WlSurface> {
    let mut surfaces: Vec<WlSurface> = state
        .xdg_shell_state
        .toplevel_surfaces()
        .iter()
        .map(|toplevel| toplevel.wl_surface().clone())
        .collect();
    surfaces.extend(state.popups.iter().map(|popup| popup.wl_surface().clone()));
    // An X window is a window like any other once it has a surface: captured,
    // encoded and paced down the same path. Everything above it differs — it is
    // configured in X and told to close in X — but none of that is pixels.
    surfaces.extend(state.x11.iter().filter_map(X11Surface::wl_surface));
    surfaces
}

/// Fire frame callbacks so every mapped client renders its next frame — but only
/// when [`FrameClock`] says the browser is ready for one.
fn tick_frame_callbacks(
    state: &Webland,
    known: &mut HashMap<ObjectId, Tracked>,
    start_time: std::time::Instant,
) {
    let now = start_time.elapsed().as_millis() as u32;
    let at = std::time::Instant::now();
    for surface in streamed_surfaces(state) {
        // A surface with no entry yet has never been captured, so nobody is
        // waiting on its frames; it gets one on the next pass.
        if let Some(tracked) = known.get_mut(&surface.id())
            && tracked.clock.should_tick(at)
        {
            send_frames_surface_tree(&surface, now);
        }
    }
}

/// Run the compositor with a winit-backed output: a window on the host desktop.
///
/// Binds a fresh `wayland-N` socket (never `wayland-0`, and distinct from the
/// session's own display), prints its name, and — if `WEBLAND_SPAWN` is set —
/// launches that command with `WAYLAND_DISPLAY` pointed at us.
///
/// # Errors
/// Returns an error if the Wayland display, socket, or winit/GL backend cannot
/// be created, or if client dispatch fails.
///
/// `on_frame`, when present, receives a [`ServerMessage`] for every surface that
/// appears and for every redraw — the seam that feeds the browser transport.
/// (The frame payloads are placeholders until per-surface capture lands; this
/// wiring proves the compositor → transport → browser path end to end.)
///
/// # Panics
/// Panics if the GL renderer fails to bind or render a frame; the winit backend
/// is assumed healthy for the lifetime of the window.
pub fn run_winit(
    on_frame: Option<Box<dyn Fn(ServerMessage)>>,
    mut poll_client: Option<Box<dyn FnMut() -> Option<ClientMessage>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut display: Display<Webland> = Display::new()?;
    let dh = display.handle();
    // An event loop only for XWayland: smithay hands its X server and window
    // manager over as calloop sources, and the compositor's own loop is a plain
    // one. Pumped with a zero timeout each pass rather than run, so the shape of
    // the main loop stays what it was.
    let mut event_loop: EventLoop<'static, Webland> = EventLoop::try_new()?;
    let xwayland_starting = xwayland::start(&event_loop.handle(), &dh);

    let compositor_state = CompositorState::new::<Webland>(&dh);
    let shm_state = ShmState::new::<Webland>(&dh, vec![]);
    // Phase 1 renders through winit's own GL context; no dmabuf global here.
    let dmabuf_state = DmabufState::new();
    let xdg_shell_state = XdgShellState::new::<Webland>(&dh);
    // The global is registered on the display, not held by the returned value,
    // and nothing here reads it back — it exists so clients can ask, and are
    // told the shell decorates.
    let _decoration = XdgDecorationState::new::<Webland>(&dh);
    // Cursor shapes by name. Without this global a client has no way to ask for
    // one, and falls back to committing a themed image as a surface — which is
    // a picture the browser is never sent, so every application would be stuck
    // with the arrow the browser draws.
    let _cursor_shape = CursorShapeManagerState::new::<Webland>(&dh);
    let relative_pointer_state = RelativePointerManagerState::new::<Webland>(&dh);
    let pointer_constraints_state = PointerConstraintsState::new::<Webland>(&dh);
    let data_device_state = DataDeviceState::new::<Webland>(&dh);
    let primary_selection_state = PrimarySelectionState::new::<Webland>(&dh);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "winit");

    let (copied, pastes) = std::sync::mpsc::channel();
    let mut state = Webland {
        compositor_state,
        xdg_shell_state,
        shm_state,
        dmabuf_state,
        seat_state,
        data_device_state,
        primary_selection_state,
        _relative_pointer_state: relative_pointer_state,
        _pointer_constraints_state: pointer_constraints_state,
        pointer_location: Point::from((0.0, 0.0)),
        output: browser_output(&dh, configured_size()),
        xwayland_shell_state: XWaylandShellState::new::<Webland>(&dh),
        xwm: None,
        xdisplay: None,
        xwayland_gone: false,
        x11: Vec::new(),
        seat,
        keyframe: false,
        focus: None,
        size: configured_size(),
        announce_applications: false,
        server_decorated: HashSet::new(),
        requests: Vec::new(),
        popups: Vec::new(),
        dh: dh.clone(),
        clipboard: String::new(),
        copied,
        pastes,
        cursor: String::from("default"),
        announce_cursor: false,
    };

    let keyboard = state
        .seat
        // 600ms before repeat, then 25 keys/s. These go to the client verbatim
        // as wl_keyboard.repeat_info and the client does the repeating, so the
        // numbers are not ours to be approximate about: the placeholder 200/200
        // asked for 200 keys a second, and a key-up that took a few frames to
        // arrive spelled out thirty characters.
        .add_keyboard(xkb_config(), 600, 25)
        .unwrap();
    let pointer = state.seat.add_pointer();

    let listener = ListeningSocket::bind_auto("wayland", 1..33)?;
    let socket_name = listener
        .socket_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or("listening socket has no name")?;
    tracing::info!(display = ?socket_name, "Webland compositor is up; point clients here");

    // Before anything is launched: an X client reads `DISPLAY` once, at startup,
    // and a client started before the X server would never see it. Skipped
    // entirely when no X server was spawned, or the wait is a timeout nothing
    // can end — five seconds of nothing on every machine without XWayland.
    if xwayland_starting {
        xwayland::wait_ready(&mut event_loop, &mut display, &mut state);
    }

    // Everything launched from here on joins this session: our socket, our X
    // display, and a session bus that is ours rather than the host's.
    let env = spawn::Env::new(&socket_name, state.xdisplay);

    if let Some(cmd) = std::env::var_os("WEBLAND_SPAWN") {
        match env.command(&cmd).spawn() {
            Ok(_) => tracing::info!(command = ?cmd, "spawned client"),
            Err(err) => tracing::warn!(command = ?cmd, %err, "failed to spawn client"),
        }
    }

    let (mut backend, mut winit) = winit::init::<GlesRenderer>()?;
    // Scanned now and re-scanned only when a directory changes: installing or
    // removing an application while the session runs has to reach the launcher,
    // but re-reading a hundred files every pass would be paying for nothing.
    let mut applications = apps::Applications::scan();
    tracing::info!(
        count = applications.listing().len(),
        "applications available"
    );

    let start_time = std::time::Instant::now();

    // Maps each live surface to its announced id and last announced size.
    let mut known: HashMap<_, Tracked> = HashMap::new();
    let mut next_surface_id: u64 = 0;
    loop {
        let status = winit.dispatch_new_events(|event| match event {
            WinitEvent::Input(BackendInputEvent::Keyboard { event }) => {
                keyboard.input::<(), _>(
                    &mut state,
                    event.key_code(),
                    event.state(),
                    0.into(),
                    0,
                    |_, _, _| FilterResult::Forward,
                );
            }
            WinitEvent::Input(BackendInputEvent::PointerMotionAbsolute { .. }) => {
                if let Some(surface) = state.xdg_shell_state.toplevel_surfaces().iter().next() {
                    let surface = surface.wl_surface().clone();
                    keyboard.set_focus(&mut state, Some(surface), 0.into());
                }
            }
            _ => {}
        });

        if let PumpStatus::Exit(_) = status {
            return Ok(());
        }

        // An application installed or removed since the last pass: the browser
        // holds the listing, so it has to be told the new one.
        if applications.refresh() {
            state.announce_applications = true;
        }

        drain_client(
            &mut state,
            &mut poll_client,
            &mut known,
            &Session {
                keyboard: &keyboard,
                pointer: &pointer,
                start_time,
                applications: &applications,
                env: &env,
            },
        );

        let size = backend.window_size();
        let damage = Rectangle::from_size(size);

        // Scoped so `framebuffer` (and the renderer borrow) drop before submit.
        {
            let (renderer, mut framebuffer) = backend.bind().unwrap();
            let elements = state
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .flat_map(|surface| {
                    render_elements_from_surface_tree(
                        renderer,
                        surface.wl_surface(),
                        (0, 0),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect::<Vec<WaylandSurfaceRenderElement<GlesRenderer>>>();

            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Flipped180)
                .unwrap();
            frame
                .clear(Color32F::new(0.1, 0.1, 0.12, 1.0), &[damage])
                .unwrap();
            draw_render_elements(&mut frame, 1.0, &elements, &[damage]).unwrap();
            let _sync = frame.finish().unwrap();
        }

        stream_dirty(
            &mut state,
            &applications,
            None,
            on_frame.as_deref(),
            &mut known,
            &mut next_surface_id,
        );
        tick_frame_callbacks(&state, &mut known, start_time);

        if let Some(stream) = listener.accept()? {
            // The handle is dropped: the display owns the connection, and the
            // `Vec` these used to be pushed into only ever grew — one entry per
            // client that had ever connected, live or long gone.
            display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))?;
        }

        display.dispatch_clients(&mut state)?;
        event_loop.dispatch(Some(std::time::Duration::ZERO), &mut state)?;
        display.flush_clients()?;

        backend.submit(Some(&[damage])).unwrap();
    }
}

/// Run the compositor headless: no local window, the browser is the only display.
///
/// A renderer-free Wayland event loop — shm clients are captured directly and
/// streamed, so no GLES/EGL context is needed. Frame callbacks are driven at
/// ~60Hz, which is what paces client rendering in the absence of an output.
///
/// # Errors
/// Returns an error if the Wayland display or socket cannot be created, or if
/// client dispatch fails.
pub fn run_headless(
    on_frame: Option<Box<dyn Fn(ServerMessage)>>,
    mut poll_client: Option<Box<dyn FnMut() -> Option<ClientMessage>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut display: Display<Webland> = Display::new()?;
    let dh = display.handle();
    // An event loop only for XWayland: smithay hands its X server and window
    // manager over as calloop sources, and the compositor's own loop is a plain
    // one. Pumped with a zero timeout each pass rather than run, so the shape of
    // the main loop stays what it was.
    let mut event_loop: EventLoop<'static, Webland> = EventLoop::try_new()?;
    let xwayland_starting = xwayland::start(&event_loop.handle(), &dh);

    let compositor_state = CompositorState::new::<Webland>(&dh);
    let shm_state = ShmState::new::<Webland>(&dh, vec![]);
    let gpu = open_gpu();
    let mut dmabuf_state = DmabufState::new();
    // Must be the v4 global, built with default feedback. A v3 global advertises
    // formats but never names a device, and Mesa's EGL Wayland platform learns
    // which DRM node to open from exactly that: without feedback it gets fd -1,
    // gives up, and the client silently falls back to wl_shm.
    if let Some((renderer, device)) = gpu.as_ref() {
        // Advertise only what the encoder can actually take. Left to itself Mesa
        // picks a compressed AMD modifier, which arrives as two planes — pixels
        // plus DCC metadata — in two buffer objects, and VA-API will only map a
        // frame made from one. Offering LINEAR alone makes the client allocate
        // something importable, so the zero-copy path is available at all.
        //
        // ponytail: LINEAR is the one modifier certain to work everywhere, at
        // the cost of the client rendering into an untiled buffer. Querying
        // VA-API for the tiled modifiers it can import would be faster for the
        // client and is the upgrade path.
        let formats: Vec<_> = renderer
            .dmabuf_formats()
            .iter()
            .filter(|format| format.modifier == Modifier::Linear)
            .copied()
            .collect();
        let feedback = DmabufFeedbackBuilder::new(*device, formats).build()?;
        dmabuf_state.create_global_with_default_feedback::<Webland>(&dh, &feedback);
    }
    let mut renderer = gpu.map(|(renderer, _)| renderer);
    let xdg_shell_state = XdgShellState::new::<Webland>(&dh);
    // The global is registered on the display, not held by the returned value,
    // and nothing here reads it back — it exists so clients can ask, and are
    // told the shell decorates.
    let _decoration = XdgDecorationState::new::<Webland>(&dh);
    // Cursor shapes by name. Without this global a client has no way to ask for
    // one, and falls back to committing a themed image as a surface — which is
    // a picture the browser is never sent, so every application would be stuck
    // with the arrow the browser draws.
    let _cursor_shape = CursorShapeManagerState::new::<Webland>(&dh);
    let relative_pointer_state = RelativePointerManagerState::new::<Webland>(&dh);
    let pointer_constraints_state = PointerConstraintsState::new::<Webland>(&dh);
    let data_device_state = DataDeviceState::new::<Webland>(&dh);
    let primary_selection_state = PrimarySelectionState::new::<Webland>(&dh);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "webland");

    let (copied, pastes) = std::sync::mpsc::channel();
    let mut state = Webland {
        compositor_state,
        xdg_shell_state,
        shm_state,
        dmabuf_state,
        seat_state,
        data_device_state,
        primary_selection_state,
        _relative_pointer_state: relative_pointer_state,
        _pointer_constraints_state: pointer_constraints_state,
        pointer_location: Point::from((0.0, 0.0)),
        output: browser_output(&dh, configured_size()),
        xwayland_shell_state: XWaylandShellState::new::<Webland>(&dh),
        xwm: None,
        xdisplay: None,
        xwayland_gone: false,
        x11: Vec::new(),
        seat,
        keyframe: false,
        focus: None,
        size: configured_size(),
        announce_applications: false,
        server_decorated: HashSet::new(),
        requests: Vec::new(),
        popups: Vec::new(),
        dh: dh.clone(),
        clipboard: String::new(),
        copied,
        pastes,
        cursor: String::from("default"),
        announce_cursor: false,
    };

    let keyboard = state
        .seat
        // 600ms before repeat, then 25 keys/s. These go to the client verbatim
        // as wl_keyboard.repeat_info and the client does the repeating, so the
        // numbers are not ours to be approximate about: the placeholder 200/200
        // asked for 200 keys a second, and a key-up that took a few frames to
        // arrive spelled out thirty characters.
        .add_keyboard(xkb_config(), 600, 25)
        .unwrap();
    let pointer = state.seat.add_pointer();

    let listener = ListeningSocket::bind_auto("wayland", 1..33)?;
    let socket_name = listener
        .socket_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or("listening socket has no name")?;
    tracing::info!(display = ?socket_name, "Webland compositor is up (headless); the browser is the display");

    // Before anything is launched: an X client reads `DISPLAY` once, at startup,
    // and a client started before the X server would never see it. Skipped
    // entirely when no X server was spawned, or the wait is a timeout nothing
    // can end — five seconds of nothing on every machine without XWayland.
    if xwayland_starting {
        xwayland::wait_ready(&mut event_loop, &mut display, &mut state);
    }

    // Everything launched from here on joins this session: our socket, our X
    // display, and a session bus that is ours rather than the host's.
    let env = spawn::Env::new(&socket_name, state.xdisplay);

    if let Some(cmd) = std::env::var_os("WEBLAND_SPAWN") {
        match env.command(&cmd).spawn() {
            Ok(_) => tracing::info!(command = ?cmd, "spawned client"),
            Err(err) => tracing::warn!(command = ?cmd, %err, "failed to spawn client"),
        }
    }

    // Scanned now and re-scanned only when a directory changes: installing or
    // removing an application while the session runs has to reach the launcher,
    // but re-reading a hundred files every pass would be paying for nothing.
    let mut applications = apps::Applications::scan();
    tracing::info!(
        count = applications.listing().len(),
        "applications available"
    );

    let start_time = std::time::Instant::now();
    let mut known: HashMap<ObjectId, Tracked> = HashMap::new();
    let mut next_surface_id: u64 = 0;
    loop {
        let pass_started = std::time::Instant::now();
        if let Some(stream) = listener.accept()? {
            // The handle is dropped: the display owns the connection, and the
            // `Vec` these used to be pushed into only ever grew — one entry per
            // client that had ever connected, live or long gone.
            display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))?;
        }
        display.dispatch_clients(&mut state)?;
        event_loop.dispatch(Some(std::time::Duration::ZERO), &mut state)?;

        // An application installed or removed since the last pass: the browser
        // holds the listing, so it has to be told the new one.
        if applications.refresh() {
            state.announce_applications = true;
        }

        drain_client(
            &mut state,
            &mut poll_client,
            &mut known,
            &Session {
                keyboard: &keyboard,
                pointer: &pointer,
                start_time,
                applications: &applications,
                env: &env,
            },
        );
        stream_dirty(
            &mut state,
            &applications,
            renderer.as_mut(),
            on_frame.as_deref(),
            &mut known,
            &mut next_surface_id,
        );
        tick_frame_callbacks(&state, &mut known, start_time);

        display.flush_clients()?;
        // Sleep out what is left of the frame, not a whole frame on top of it.
        // A flat 16ms made the pass cost 16ms *plus* dispatch, capture, encode
        // and transport, so the desktop never reached 60Hz and every keystroke
        // waited on a sleep that had nothing to do with it.
        if let Some(rest) = FRAME_INTERVAL.checked_sub(pass_started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

impl OutputHandler for Webland {}

delegate_compositor!(Webland);
delegate_output!(Webland);
delegate_xwayland_shell!(Webland);
delegate_xdg_shell!(Webland);
delegate_xdg_decoration!(Webland);
delegate_cursor_shape!(Webland);
delegate_shm!(Webland);
delegate_dmabuf!(Webland);
delegate_seat!(Webland);
delegate_data_device!(Webland);
delegate_primary_selection!(Webland);
delegate_pointer_constraints!(Webland);
delegate_relative_pointer!(Webland);

#[cfg(test)]
mod tests {
    use super::{
        FrameClock, IDLE_FRAME_INTERVAL, INITIAL_FRAME_CREDIT, MAX_FRAME_CREDIT, ancestry,
        changed_region, clip, crop, pointer_surface, preferred_mime,
    };
    use std::time::{Duration, Instant};
    use webland_core::{Point, Rect, Size, SurfaceId};
    use webland_protocol::{InputEvent, Press};

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// Motion and scroll go to the window the browser delivered them to;
    /// everything else follows the keyboard focus. Routing all of it to the
    /// focus meant hovering or scrolling an unfocused window moved the pointer
    /// inside the focused one instead.
    #[test]
    fn only_motion_and_scroll_name_the_surface_they_go_to() {
        let hovered = SurfaceId(4);
        assert_eq!(
            pointer_surface(&InputEvent::PointerMotion {
                id: hovered,
                position: Point { x: 3.0, y: 4.0 },
            }),
            Some(hovered)
        );
        assert_eq!(
            pointer_surface(&InputEvent::PointerScroll {
                id: hovered,
                dx: 0.0,
                dy: -120.0,
            }),
            Some(hovered)
        );
        // A button has no surface of its own in Wayland: it goes to the
        // pointer's focus, which the motion above has already set. So do keys,
        // and so does locked relative motion.
        for event in [
            InputEvent::PointerButton {
                button: 0x110,
                state: Press::Down,
            },
            InputEvent::Key {
                keycode: 30,
                state: Press::Down,
            },
            InputEvent::PointerMotionRelative { dx: 1.0, dy: 1.0 },
        ] {
            assert_eq!(pointer_surface(&event), None);
        }
    }

    #[test]
    fn preferred_mime_asks_for_text_and_nothing_else() {
        let offers = |mimes: &[&str]| mimes.iter().map(|m| (*m).to_string()).collect::<Vec<_>>();
        // The utf-8 spelling wins even when it is offered second.
        assert_eq!(
            preferred_mime(&offers(&["text/plain", "text/plain;charset=utf-8"])),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(preferred_mime(&offers(&["text/plain"])), Some("text/plain"));
        // A copied image or file list is not something to put on the clipboard
        // of a browser, so nothing is asked for at all.
        assert_eq!(
            preferred_mime(&offers(&["image/png", "text/uri-list"])),
            None
        );
        assert_eq!(preferred_mime(&[]), None);
    }

    #[test]
    fn ancestry_reaches_the_root_and_refuses_to_loop() {
        // A submenu of a menu of a window: clicking the submenu must leave all
        // three standing, which is what keeping the whole chain is for.
        let parents = |id: &u32| match id {
            3 => Some(2),
            2 => Some(1),
            _ => None,
        };
        assert_eq!(ancestry(Some(3), parents), vec![3, 2, 1]);
        assert_eq!(ancestry(Some(1), parents), vec![1]);
        assert!(ancestry(None::<u32>, parents).is_empty());
        // A client that describes a cycle gets a finite answer, not a hang.
        assert_eq!(
            ancestry(Some(9), |id: &u32| Some(if *id == 9 { 8 } else { 9 })),
            vec![9, 8]
        );
    }

    #[test]
    fn clip_keeps_the_window_inside_the_image() {
        let image = Size {
            width: 100,
            height: 80,
        };
        // The ordinary case: a client that drew a 10px shadow all round.
        assert_eq!(clip((10, 10), (80, 60), image), rect(10, 10, 80, 60));
        // A geometry larger than what was committed is trimmed, not trusted.
        assert_eq!(clip((10, 10), (200, 200), image), rect(10, 10, 90, 70));
        // Nonsense stays inside the image and stays visible: an empty rectangle
        // would be a window drawn at zero pixels, which reads as a lost window.
        assert_eq!(clip((-5, -5), (50, 40), image), rect(0, 0, 50, 40));
        assert_eq!(clip((500, 500), (50, 40), image), rect(100, 80, 1, 1));
        assert_eq!(clip((0, 0), (0, 0), image), rect(0, 0, 1, 1));
    }

    #[test]
    fn changed_region_bounds_only_what_differs() {
        let size = Size {
            width: 4,
            height: 3,
        };
        let old = vec![0u8; 4 * 3 * 4];
        assert_eq!(changed_region(&old, &old, size), None);

        // One pixel, at (2, 1): row 1 of 4 pixels, then 2 pixels in.
        let mut new = old.clone();
        new[(4 + 2) * 4] = 9;
        assert_eq!(changed_region(&old, &new, size), Some(rect(2, 1, 1, 1)));

        // Two apart: the box spans them, rows and columns both.
        new[(2 * 4) * 4 + 3] = 9;
        assert_eq!(changed_region(&old, &new, size), Some(rect(0, 1, 3, 2)));

        // A resize is not a diff; the caller sends the whole surface instead.
        assert_eq!(changed_region(&old, &new[..8], size), None);
    }

    #[test]
    fn crop_packs_rows_and_refuses_to_read_past_the_buffer() {
        // 4x2 BGRA, each pixel byte = its row number, with 4 bytes of padding
        // per row so the stride is not the width.
        let stride: i32 = 4 * 4 + 4;
        let mut buffer = vec![0u8; stride as usize * 2];
        buffer[stride as usize..stride as usize + 16].fill(1);

        let packed = crop(&buffer, 0, stride, rect(1, 1, 2, 1));
        assert_eq!(packed, vec![1u8; 8]);
        // Short buffer: return nothing rather than read off the end.
        assert!(crop(&buffer[..stride as usize], 0, stride, rect(0, 1, 4, 1)).is_empty());
    }

    #[test]
    fn frame_clock_spends_credit_then_waits_for_the_browser() {
        let now = Instant::now();
        let mut clock = FrameClock::new();

        // Clients may render a little before any browser has presented.
        for _ in 0..INITIAL_FRAME_CREDIT {
            assert!(clock.should_tick(now));
        }
        // Out of credit: no more callbacks until the browser catches up, so the
        // client cannot run ahead into frames that will be discarded.
        assert!(!clock.should_tick(now));

        clock.on_ack(now);
        assert!(clock.should_tick(now));
        assert!(!clock.should_tick(now));
    }

    #[test]
    fn frame_clock_keeps_clients_alive_with_no_browser() {
        let start = Instant::now();
        let mut clock = FrameClock::new();
        for _ in 0..INITIAL_FRAME_CREDIT {
            assert!(clock.should_tick(start));
        }
        assert!(!clock.should_tick(start));

        // Without this, a compositor whose browser never connects would stall
        // every client forever: no render, no frame, no ack, no credit.
        assert!(clock.should_tick(start + IDLE_FRAME_INTERVAL));
        assert!(!clock.should_tick(start + IDLE_FRAME_INTERVAL));
    }

    #[test]
    fn frame_clock_caps_banked_credit() {
        let now = Instant::now();
        let mut clock = FrameClock::new();
        // A burst of acks (or a browser reconnecting) must not let clients
        // free-run afterwards. Nothing was sent, so no round trip is measured
        // and the ceiling stays where it started.
        for _ in 0..50 {
            clock.on_ack(now);
        }
        for _ in 0..INITIAL_FRAME_CREDIT {
            assert!(clock.should_tick(now));
        }
        assert!(!clock.should_tick(now));
    }

    #[test]
    fn frame_clock_opens_up_over_a_slow_link() {
        let now = Instant::now();
        let mut clock = FrameClock::new();
        // A 96ms round trip fits six 16ms frames in the pipe, so holding the
        // client to two would cap it at a fraction of the rate it could run.
        let trip = Duration::from_millis(96);
        for _ in 0..12 {
            clock.on_send(now);
            clock.on_ack(now + trip);
        }
        let mut ticks = 0;
        while clock.should_tick(now) {
            ticks += 1;
        }
        assert!(
            ticks > INITIAL_FRAME_CREDIT,
            "a slow link should allow more frames in flight, got {ticks}"
        );
        assert!(ticks <= MAX_FRAME_CREDIT, "but never more than the bound");
    }
}
