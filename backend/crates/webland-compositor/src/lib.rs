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

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::gbm::GbmDevice;
pub mod encode;

use smithay::backend::allocator::{Buffer, Fourcc, Modifier};
use smithay::backend::egl::{EGLContext, EGLDisplay};
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
use smithay::backend::renderer::{ExportMem, ImportDma};
use smithay::backend::winit::{self, WinitEvent};
use smithay::input::keyboard::{FilterResult, KeyboardHandle, XkbConfig};
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
use smithay::wayland::dmabuf::{
    DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier, get_dmabuf,
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
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_seat, delegate_shm,
    delegate_xdg_shell,
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
    seat: Seat<Self>,
    /// Set by the browser on connect: send whole surfaces on the next frame,
    /// because a joiner has nothing for a damage rectangle to land on.
    keyframe: bool,
    /// The surface the browser last raised, which is where input goes.
    focus: Option<SurfaceId>,
    /// The size to configure toplevels at, as the browser last reported it.
    size: (i32, i32),
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
        // Headless has no output, so clients have no size to render at and pick
        // a small default. Tell them the browser's, which `ClientMessage::Resize`
        // keeps current; `WEBLAND_SIZE` is only the value before one arrives.
        let (width, height) = self.size;
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
    match capture_shm(surface) {
        Some(captured) => Some(captured),
        None => capture_dmabuf(renderer?, surface),
    }
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
fn drain_client(
    state: &mut Webland,
    poll_client: &mut Option<Box<dyn FnMut() -> Option<ClientMessage>>>,
    known: &mut HashMap<ObjectId, Tracked>,
    keyboard: &KeyboardHandle<Webland>,
    pointer: &PointerHandle<Webland>,
    start_time: std::time::Instant,
) {
    let Some(poll) = poll_client.as_mut() else {
        return;
    };
    let mut events = Vec::new();
    let mut resize = None;
    let mut closing = Vec::new();
    while let Some(message) = poll() {
        match message {
            ClientMessage::Input(event) => events.push(event),
            ClientMessage::FramePresented { id } => {
                if let Some(tracked) = known.values_mut().find(|tracked| tracked.id == id) {
                    tracked.clock.on_ack(std::time::Instant::now());
                }
            }
            ClientMessage::RequestKeyframe => state.keyframe = true,
            ClientMessage::Focus { id } => state.focus = Some(id),
            ClientMessage::Resize { size } => resize = Some(size),
            ClientMessage::CloseSurface { id } => closing.push(id),
        }
    }
    for id in closing {
        let object = known
            .iter()
            .find(|(_, tracked)| tracked.id == id)
            .map(|(object, _)| object.clone());
        if let Some(toplevel) = state
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| Some(toplevel.wl_surface().id()) == object)
        {
            toplevel.send_close();
        }
    }

    // Reconfigure every toplevel when the browser's window changes size. The
    // client redraws at the new size and the next capture picks it up, which is
    // what turns a browser resize into a sharp surface rather than a scaled one.
    if let Some(size) = resize {
        #[allow(clippy::cast_possible_wrap)]
        let wanted = (size.width.max(1) as i32, size.height.max(1) as i32);
        if state.size != wanted {
            state.size = wanted;
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
    let focused = state.focus.and_then(|id| {
        known
            .iter()
            .find(|(_, tracked)| tracked.id == id)
            .map(|(object, _)| object.clone())
    });
    let toplevels = state.xdg_shell_state.toplevel_surfaces();
    let target = toplevels
        .iter()
        .find(|toplevel| Some(toplevel.wl_surface().id()) == focused)
        .or_else(|| toplevels.first())
        .map(|toplevel| toplevel.wl_surface().clone());
    if !events.is_empty()
        && let Some(surface) = target
    {
        let now = start_time.elapsed().as_millis() as u32;
        // Only when it actually changes: re-focusing what is already focused
        // makes smithay resend `enter` and `modifiers` for nothing.
        if keyboard.current_focus().as_ref() != Some(&surface) {
            keyboard.set_focus(state, Some(surface.clone()), SERIAL_COUNTER.next_serial());
        }
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
}

/// Capture changed surfaces and emit their frames to the browser transport.
///
/// Only pixels that actually changed go on the wire, so an idle surface costs
/// nothing at all. A browser joining mid-stream has nothing for a damage
/// rectangle to land on, so it asks for a keyframe and gets whole surfaces once.
fn stream_dirty(
    state: &mut Webland,
    mut renderer: Option<&mut GlesRenderer>,
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
                encoder: None,
                clock: FrameClock::new(),
                title: None,
            }
        });
        let Some(commit) = with_renderer_surface_state(surface, |s| s.current_commit()) else {
            continue;
        };
        if tracked.commit == Some(commit) && !keyframe {
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
        tracked.commit = Some(commit);

        // A resize invalidates whatever the browser is holding, and so does a
        // browser that has just joined: both take the whole surface.
        let resized = tracked.size != Some(size);
        let announced = keyframe || resized;
        if announced {
            tracked.size = Some(size);
            emit(ServerMessage::SurfaceCreated(SurfaceCreated {
                id: tracked.id,
                size,
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
        }
        let title = toplevel_title(surface);
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
        }
        alive
    });
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

/// Fire frame callbacks so every mapped client renders its next frame — but only
/// when [`FrameClock`] says the browser is ready for one.
fn tick_frame_callbacks(
    state: &Webland,
    known: &mut HashMap<ObjectId, Tracked>,
    start_time: std::time::Instant,
) {
    let now = start_time.elapsed().as_millis() as u32;
    let at = std::time::Instant::now();
    for surface in state.xdg_shell_state.toplevel_surfaces() {
        let wl_surface = surface.wl_surface();
        // A surface with no entry yet has never been captured, so nobody is
        // waiting on its frames; it gets one on the next pass.
        if let Some(tracked) = known.get_mut(&wl_surface.id())
            && tracked.clock.should_tick(at)
        {
            send_frames_surface_tree(wl_surface, now);
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

    let compositor_state = CompositorState::new::<Webland>(&dh);
    let shm_state = ShmState::new::<Webland>(&dh, vec![]);
    // Phase 1 renders through winit's own GL context; no dmabuf global here.
    let dmabuf_state = DmabufState::new();
    let xdg_shell_state = XdgShellState::new::<Webland>(&dh);
    let data_device_state = DataDeviceState::new::<Webland>(&dh);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "winit");

    let mut state = Webland {
        compositor_state,
        xdg_shell_state,
        shm_state,
        dmabuf_state,
        seat_state,
        data_device_state,
        seat,
        keyframe: false,
        focus: None,
        size: configured_size(),
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
            &mut known,
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
            None,
            on_frame.as_deref(),
            &mut known,
            &mut next_surface_id,
        );
        tick_frame_callbacks(&state, &mut known, start_time);

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
    let data_device_state = DataDeviceState::new::<Webland>(&dh);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "webland");

    let mut state = Webland {
        compositor_state,
        xdg_shell_state,
        shm_state,
        dmabuf_state,
        seat_state,
        data_device_state,
        seat,
        keyframe: false,
        focus: None,
        size: configured_size(),
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
            &mut known,
            &keyboard,
            &pointer,
            start_time,
        );
        stream_dirty(
            &mut state,
            renderer.as_mut(),
            on_frame.as_deref(),
            &mut known,
            &mut next_surface_id,
        );
        tick_frame_callbacks(&state, &mut known, start_time);

        display.flush_clients()?;
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

delegate_compositor!(Webland);
delegate_xdg_shell!(Webland);
delegate_shm!(Webland);
delegate_dmabuf!(Webland);
delegate_seat!(Webland);
delegate_data_device!(Webland);

#[cfg(test)]
mod tests {
    use super::{
        FrameClock, IDLE_FRAME_INTERVAL, INITIAL_FRAME_CREDIT, MAX_FRAME_CREDIT, changed_region,
        crop,
    };
    use std::time::{Duration, Instant};
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
