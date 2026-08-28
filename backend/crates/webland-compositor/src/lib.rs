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

use std::collections::{HashMap, HashSet};
use std::os::unix::io::OwnedFd;
use std::sync::Arc;

use webland_core::{Size, SurfaceId};
use webland_protocol::{Codec, InputEvent, Press, ServerMessage, SurfaceCreated, SurfaceFrame};

use smithay::backend::input::{
    ButtonState, InputEvent as BackendInputEvent, KeyState, KeyboardKeyEvent, Keycode,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::{
    draw_render_elements, on_commit_buffer_handler, with_renderer_surface_state,
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
    /// Surfaces committed since the last frame was streamed (damage tracking).
    dirty: HashSet<ObjectId>,
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
        on_commit_buffer_handler::<Self>(surface);
        self.dirty.insert(surface.id());
    }
}

impl XdgShellHandler for Webland {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        tracing::info!("new xdg toplevel mapped");
        surface.with_pending_state(|state| {
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

/// Re-announce every surface roughly once a second so a browser that connects
/// mid-stream still learns each surface's size (the broadcast has no history).
const REANNOUNCE_EVERY: u64 = 120;

/// Copy a surface's currently-committed `wl_shm` contents into a raw frame.
///
/// Returns `None` until the surface has an shm buffer (dmabuf clients take the
/// zero-copy path described in Decision 2, which is not wired here yet).
fn capture_shm(surface: &WlSurface) -> Option<(Size, Vec<u8>)> {
    with_renderer_surface_state(surface, |renderer_state| {
        let buffer = renderer_state.buffer()?;
        with_buffer_contents(buffer, |ptr, len, data| {
            let size = Size {
                width: data.width as u32,
                height: data.height as u32,
            };
            // SAFETY: smithay guarantees `ptr` addresses `len` initialized bytes
            // of the shm pool mapping for the duration of this callback.
            #[allow(unsafe_code)]
            let pixels = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
            (size, pixels)
        })
        .ok()
    })
    .flatten()
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

/// Drain browser input and inject it into the seat, targeting the first surface.
fn drain_input(
    state: &mut Webland,
    poll_input: &mut Option<Box<dyn FnMut() -> Option<InputEvent>>>,
    keyboard: &KeyboardHandle<Webland>,
    pointer: &PointerHandle<Webland>,
    start_time: std::time::Instant,
) {
    let Some(poll) = poll_input.as_mut() else {
        return;
    };
    let mut events = Vec::new();
    while let Some(event) = poll() {
        events.push(event);
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

/// Capture changed surfaces and emit their frames to the browser transport.
fn stream_dirty(
    state: &mut Webland,
    on_frame: Option<&dyn Fn(ServerMessage)>,
    known: &mut HashMap<ObjectId, (SurfaceId, Option<Size>)>,
    next_surface_id: &mut u64,
    tick: &mut u64,
) {
    let Some(emit) = on_frame else {
        return;
    };
    *tick = tick.wrapping_add(1);
    let reannounce = tick.is_multiple_of(REANNOUNCE_EVERY);
    // Snapshot toplevels so we can consult and clear `state.dirty` freely.
    let toplevels: Vec<WlSurface> = state
        .xdg_shell_state
        .toplevel_surfaces()
        .iter()
        .map(|toplevel| toplevel.wl_surface().clone())
        .collect();
    for surface in &toplevels {
        let object = surface.id();
        // Stream only surfaces that changed, plus a periodic keyframe so a browser
        // that connects mid-stream still gets current pixels.
        let changed = state.dirty.remove(&object);
        if !(changed || reannounce) {
            continue;
        }
        let Some((surface_size, pixels)) = capture_shm(surface) else {
            continue;
        };
        let entry = known.entry(object).or_insert_with(|| {
            let id = SurfaceId(*next_surface_id);
            *next_surface_id += 1;
            (id, None)
        });
        let id = entry.0;
        if reannounce || entry.1 != Some(surface_size) {
            entry.1 = Some(surface_size);
            emit(ServerMessage::SurfaceCreated(SurfaceCreated {
                id,
                size: surface_size,
            }));
        }
        emit(ServerMessage::SurfaceFrame(SurfaceFrame {
            id,
            codec: Codec::Deflate,
            damage: Vec::new(),
            payload: webland_protocol::deflate(&pixels),
        }));
    }
}

/// Fire frame callbacks so every mapped client renders its next frame.
fn tick_frame_callbacks(state: &Webland, start_time: std::time::Instant) {
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
    mut poll_input: Option<Box<dyn FnMut() -> Option<InputEvent>>>,
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
        dirty: HashSet::new(),
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
    let mut known: HashMap<_, (SurfaceId, Option<Size>)> = HashMap::new();
    let mut next_surface_id: u64 = 0;
    let mut tick: u64 = 0;

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

        drain_input(&mut state, &mut poll_input, &keyboard, &pointer, start_time);

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
            &mut tick,
        );
        tick_frame_callbacks(&state, start_time);

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
    mut poll_input: Option<Box<dyn FnMut() -> Option<InputEvent>>>,
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
        dirty: HashSet::new(),
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
    let mut known: HashMap<ObjectId, (SurfaceId, Option<Size>)> = HashMap::new();
    let mut next_surface_id: u64 = 0;
    let mut tick: u64 = 0;

    loop {
        if let Some(stream) = listener.accept()? {
            let client = display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))?;
            clients.push(client);
        }
        display.dispatch_clients(&mut state)?;

        drain_input(&mut state, &mut poll_input, &keyboard, &pointer, start_time);
        stream_dirty(
            &mut state,
            on_frame.as_deref(),
            &mut known,
            &mut next_surface_id,
            &mut tick,
        );
        tick_frame_callbacks(&state, start_time);

        display.flush_clients()?;
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

delegate_compositor!(Webland);
delegate_xdg_shell!(Webland);
delegate_shm!(Webland);
delegate_seat!(Webland);
delegate_data_device!(Webland);
