//! Wayland compositor for Webland.
//!
//! Built on [`smithay`]. Wayland-first: `XWayland` support, if it ever lands,
//! goes behind a feature flag rather than into this module.
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
    clippy::needless_pass_by_value
)]

/// Re-exported so downstream crates pin one Wayland stack.
pub use smithay;

use std::collections::HashMap;
use std::os::unix::io::OwnedFd;
use std::sync::Arc;

use webland_core::{Rect, Size, SurfaceId};
use webland_protocol::{
    ClientMessage, Codec, InputEvent, Press, ServerMessage, SurfaceCreated, SurfaceFrame,
};

use smithay::backend::input::{
    ButtonState, InputEvent as BackendInputEvent, KeyState, KeyboardKeyEvent, Keycode,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::{
    CommitCounter, draw_render_elements, on_commit_buffer_handler, with_renderer_surface_state,
};
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit::{self, WinitEvent};
use smithay::input::keyboard::{FilterResult, KeyboardHandle};
use smithay::input::pointer::{ButtonEvent, MotionEvent, PointerHandle};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{
    ClientData, ClientId, DisconnectReason, ObjectId,
};
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::reexports::wayland_server::protocol::wl_seat;
use smithay::reexports::wayland_server::protocol::wl_surface::{self, WlSurface};
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket, Resource};
use smithay::reexports::winit::platform::pump_events::PumpStatus;
use smithay::utils::{Rectangle, SERIAL_COUNTER, Serial, Transform};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes, TraversalAction,
    with_surface_tree_downward,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState, with_buffer_contents};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_seat, delegate_shm, delegate_xdg_shell,
};

/// Compositor state. Holds the protocol globals and the seat; owns everything a
/// Wayland client interacts with.
#[derive(Debug)]
pub struct Webland {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    seat: Seat<Self>,
    /// Set by the browser on connect: send whole surfaces on the next frame,
    /// because a joiner has nothing for a damage rectangle to land on.
    keyframe: bool,
}

impl BufferHandler for Webland {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl CompositorHandler for Webland {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
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
        // Headless has no output, so clients have no size to render at and pick a
        // small default. Tell them one, via WEBLAND_SIZE=WxH (default 1280x800).
        let (width, height) = configured_size();
        tracing::info!(width, height, "new xdg toplevel mapped");
        surface.with_pending_state(|state| {
            state.size = Some((width, height).into());
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

impl ShmHandler for Webland {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for Webland {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
    }
}

impl SelectionHandler for Webland {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Webland {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
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
        InputEvent::PointerMotion { position } => {
            // The single surface sits at the origin: surface-local == compositor.
            pointer.motion(
                state,
                Some((surface.clone(), (0.0, 0.0).into())),
                &MotionEvent {
                    location: (position.x, position.y).into(),
                    serial,
                    time,
                },
            );
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
            keyboard.input::<(), _>(state, code, to_key_state(press), serial, time, |_, _, _| {
                FilterResult::Forward
            });
        }
        InputEvent::PointerScroll { .. } => {} // axis events: a later step
    }
}

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

/// Paces `wl_surface.frame` callbacks against the browser (Decision 3).
///
/// Wayland clients redraw only when the compositor fires their frame callback.
/// Firing on the compositor's own loop rate lets a client run ahead of a browser
/// that cannot keep up: it renders into frames the pacer then discards, and
/// latency grows without bound. So a callback costs credit, and credit comes
/// from the browser saying it presented.
struct FrameClock {
    credit: i32,
    last_tick: std::time::Instant,
}

impl FrameClock {
    fn new() -> Self {
        Self {
            credit: INITIAL_FRAME_CREDIT,
            last_tick: std::time::Instant::now(),
        }
    }

    /// The browser presented a frame, so one more redraw is warranted.
    ///
    /// Capped: a burst of acks (or a browser reconnecting) must not bank enough
    /// credit for clients to free-run afterwards.
    // ponytail: one ack buys one callback tick across every surface, which is
    // exact only while there is one surface. Per-surface credit lands with the
    // multi-surface scene (Phase 4).
    fn on_ack(&mut self) {
        self.credit = (self.credit + 1).min(INITIAL_FRAME_CREDIT);
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
fn configured_size() -> (i32, i32) {
    std::env::var("WEBLAND_SIZE")
        .ok()
        .and_then(|value| {
            let (w, h) = value.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        })
        .unwrap_or((1280, 800))
}

/// Copy a surface's committed `wl_shm` contents, tightly packed.
///
/// Returns `None` until the surface has an shm buffer (dmabuf clients take the
/// zero-copy path described in Decision 2, which is not wired here yet).
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
fn drain_client(
    state: &mut Webland,
    poll_client: &mut Option<Box<dyn FnMut() -> Option<ClientMessage>>>,
    clock: &mut FrameClock,
    keyboard: &KeyboardHandle<Webland>,
    pointer: &PointerHandle<Webland>,
    start_time: std::time::Instant,
) {
    let Some(poll) = poll_client.as_mut() else {
        return;
    };
    let mut events = Vec::new();
    while let Some(message) = poll() {
        match message {
            ClientMessage::Input(event) => events.push(event),
            ClientMessage::FramePresented => clock.on_ack(),
            ClientMessage::RequestKeyframe => state.keyframe = true,
        }
    }
    if !events.is_empty()
        && let Some(surface) = state
            .xdg_shell_state
            .toplevel_surfaces()
            .first()
            .map(|toplevel| toplevel.wl_surface().clone())
    {
        let now = start_time.elapsed().as_millis() as u32;
        keyboard.set_focus(state, Some(surface.clone()), SERIAL_COUNTER.next_serial());
        for event in events {
            inject_input(state, pointer, keyboard, &surface, event, now);
        }
    }
}

/// What the browser has been told about one surface.
struct Tracked {
    id: SurfaceId,
    size: Option<Size>,
    /// The commit the browser's pixels came from, so an untouched surface costs
    /// nothing to skip.
    commit: Option<CommitCounter>,
    /// The pixels the browser is holding, to diff the next capture against.
    pixels: Vec<u8>,
}

/// Capture changed surfaces and emit their frames to the browser transport.
///
/// Only pixels that actually changed go on the wire, so an idle surface costs
/// nothing at all. A browser joining mid-stream has nothing for a damage
/// rectangle to land on, so it asks for a keyframe and gets whole surfaces once.
fn stream_dirty(
    state: &mut Webland,
    on_frame: Option<&dyn Fn(ServerMessage)>,
    known: &mut HashMap<ObjectId, Tracked>,
    next_surface_id: &mut u64,
) {
    let Some(emit) = on_frame else {
        return;
    };
    let keyframe = std::mem::take(&mut state.keyframe);
    let toplevels: Vec<WlSurface> = state
        .xdg_shell_state
        .toplevel_surfaces()
        .iter()
        .map(|toplevel| toplevel.wl_surface().clone())
        .collect();
    for surface in &toplevels {
        let tracked = known.entry(surface.id()).or_insert_with(|| {
            let id = SurfaceId(*next_surface_id);
            *next_surface_id += 1;
            Tracked {
                id,
                size: None,
                commit: None,
                pixels: Vec::new(),
            }
        });
        let Some(commit) = with_renderer_surface_state(surface, |s| s.current_commit()) else {
            continue;
        };
        if tracked.commit == Some(commit) && !keyframe {
            continue;
        }
        let Some((size, pixels)) = capture_shm(surface) else {
            continue;
        };
        if pixels.is_empty() {
            continue;
        }
        tracked.commit = Some(commit);

        // A resize invalidates whatever the browser is holding, and so does a
        // browser that has just joined: both take the whole surface.
        let whole = Rect {
            x: 0,
            y: 0,
            width: size.width,
            height: size.height,
        };
        let region = if keyframe || tracked.size != Some(size) {
            tracked.size = Some(size);
            emit(ServerMessage::SurfaceCreated(SurfaceCreated {
                id: tracked.id,
                size,
            }));
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
    }
}

/// Fire frame callbacks so every mapped client renders its next frame — but only
/// when [`FrameClock`] says the browser is ready for one.
fn tick_frame_callbacks(state: &Webland, start_time: std::time::Instant, clock: &mut FrameClock) {
    if !clock.should_tick(std::time::Instant::now()) {
        return;
    }
    let now = start_time.elapsed().as_millis() as u32;
    for surface in state.xdg_shell_state.toplevel_surfaces() {
        send_frames_surface_tree(surface.wl_surface(), now);
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

    let compositor_state = CompositorState::new::<Webland>(&dh);
    let shm_state = ShmState::new::<Webland>(&dh, vec![]);
    let xdg_shell_state = XdgShellState::new::<Webland>(&dh);
    let data_device_state = DataDeviceState::new::<Webland>(&dh);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "winit");

    let mut state = Webland {
        compositor_state,
        xdg_shell_state,
        shm_state,
        seat_state,
        data_device_state,
        seat,
        keyframe: false,
    };

    let keyboard = state
        .seat
        .add_keyboard(Default::default(), 200, 200)
        .unwrap();
    let pointer = state.seat.add_pointer();

    let listener = ListeningSocket::bind_auto("wayland", 1..33)?;
    let socket_name = listener
        .socket_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or("listening socket has no name")?;
    tracing::info!(display = ?socket_name, "Webland compositor is up; point clients here");

    if let Some(cmd) = std::env::var_os("WEBLAND_SPAWN") {
        match std::process::Command::new(&cmd)
            .env("WAYLAND_DISPLAY", &socket_name)
            .spawn()
        {
            Ok(_) => tracing::info!(command = ?cmd, "spawned client"),
            Err(err) => tracing::warn!(command = ?cmd, %err, "failed to spawn client"),
        }
    }

    let (mut backend, mut winit) = winit::init::<GlesRenderer>()?;
    let start_time = std::time::Instant::now();
    let mut clients = Vec::new();

    // Maps each live surface to its announced id and last announced size.
    let mut known: HashMap<_, Tracked> = HashMap::new();
    let mut next_surface_id: u64 = 0;
    let mut clock = FrameClock::new();

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

        drain_client(
            &mut state,
            &mut poll_client,
            &mut clock,
            &keyboard,
            &pointer,
            start_time,
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
            on_frame.as_deref(),
            &mut known,
            &mut next_surface_id,
        );
        tick_frame_callbacks(&state, start_time, &mut clock);

        if let Some(stream) = listener.accept()? {
            let client = display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))?;
            clients.push(client);
        }

        display.dispatch_clients(&mut state)?;
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

    let compositor_state = CompositorState::new::<Webland>(&dh);
    let shm_state = ShmState::new::<Webland>(&dh, vec![]);
    let xdg_shell_state = XdgShellState::new::<Webland>(&dh);
    let data_device_state = DataDeviceState::new::<Webland>(&dh);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "webland");

    let mut state = Webland {
        compositor_state,
        xdg_shell_state,
        shm_state,
        seat_state,
        data_device_state,
        seat,
        keyframe: false,
    };

    let keyboard = state
        .seat
        .add_keyboard(Default::default(), 200, 200)
        .unwrap();
    let pointer = state.seat.add_pointer();

    let listener = ListeningSocket::bind_auto("wayland", 1..33)?;
    let socket_name = listener
        .socket_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or("listening socket has no name")?;
    tracing::info!(display = ?socket_name, "Webland compositor is up (headless); the browser is the display");

    if let Some(cmd) = std::env::var_os("WEBLAND_SPAWN") {
        match std::process::Command::new(&cmd)
            .env("WAYLAND_DISPLAY", &socket_name)
            .spawn()
        {
            Ok(_) => tracing::info!(command = ?cmd, "spawned client"),
            Err(err) => tracing::warn!(command = ?cmd, %err, "failed to spawn client"),
        }
    }

    let start_time = std::time::Instant::now();
    let mut clients = Vec::new();
    let mut known: HashMap<ObjectId, Tracked> = HashMap::new();
    let mut next_surface_id: u64 = 0;
    let mut clock = FrameClock::new();

    loop {
        if let Some(stream) = listener.accept()? {
            let client = display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))?;
            clients.push(client);
        }
        display.dispatch_clients(&mut state)?;

        drain_client(
            &mut state,
            &mut poll_client,
            &mut clock,
            &keyboard,
            &pointer,
            start_time,
        );
        stream_dirty(
            &mut state,
            on_frame.as_deref(),
            &mut known,
            &mut next_surface_id,
        );
        tick_frame_callbacks(&state, start_time, &mut clock);

        display.flush_clients()?;
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

delegate_compositor!(Webland);
delegate_xdg_shell!(Webland);
delegate_shm!(Webland);
delegate_seat!(Webland);
delegate_data_device!(Webland);

#[cfg(test)]
mod tests {
    use super::{FrameClock, IDLE_FRAME_INTERVAL, INITIAL_FRAME_CREDIT, changed_region, crop};
    use std::time::Instant;
    use webland_core::{Rect, Size};

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
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

        clock.on_ack();
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
        // free-run afterwards.
        for _ in 0..50 {
            clock.on_ack();
        }
        for _ in 0..INITIAL_FRAME_CREDIT {
            assert!(clock.should_tick(now));
        }
        assert!(!clock.should_tick(now));
    }
}
