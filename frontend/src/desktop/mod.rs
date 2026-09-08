//! The Webland shell: window chrome, and the plumbing that connects the backend
//! transport to the scene.
//!
//! Windows are Leptos components over [`Scene`]'s signal, so dragging one or
//! raising it is a signal update and a restyle — no server round trip, no
//! re-encode. The canvas inside each window is the only part the compositor
//! knows about.

use std::cell::Cell;
use std::rc::Rc;

use leptos::html::{Canvas, Div, Main};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::PointerEvent;
use webland_core::{Size, SurfaceId};

use crate::latency::Latency;
use crate::protocol::{
    ClientMessage, ServerMessage, Transport, WebSocketTransport, decode, encode,
};
use crate::scene::Scene;

/// Path the protocol socket is served on, proxied to the backend by whatever is
/// serving the page (see `Trunk.toml`).
const BACKEND_PATH: &str = "/ws";

/// The protocol socket's URL, on the same origin as the page.
///
/// Derived rather than hardcoded so the page works wherever it is served from:
/// a hardcoded `127.0.0.1` is the viewer's own loopback once the browser is on
/// another machine, and `wss` is required on an https page.
fn backend() -> String {
    let Some(location) = web_sys::window().map(|window| window.location()) else {
        return format!("ws://127.0.0.1:9001{BACKEND_PATH}");
    };
    let secure = location.protocol().is_ok_and(|scheme| scheme == "https:");
    let scheme = if secure { "wss" } else { "ws" };
    let host = location
        .host()
        .unwrap_or_else(|_| String::from("127.0.0.1:3030"));
    format!("{scheme}://{host}{BACKEND_PATH}")
}

/// How much of the desktop a newly opened window takes up.
const WINDOW_FRACTION: f64 = 0.62;

/// The size to configure client windows at, in device pixels.
///
/// Device pixels, not CSS pixels: on a HiDPI screen each CSS pixel covers
/// `devicePixelRatio` real ones, so a surface rendered in CSS pixels gets
/// stretched to fit and looks soft. Rendering at the real count is what keeps it
/// sharp.
///
/// A fraction of the browser window rather than all of it, because these are
/// windows on a desktop now — at full size every one would cover the shell and
/// each other, and there would be nothing to drag.
fn window_size() -> Option<Size> {
    let window = web_sys::window()?;
    let ratio = crate::scene::pixel_ratio();
    let width = window.inner_width().ok()?.as_f64()? * ratio * WINDOW_FRACTION;
    let height = window.inner_height().ok()?.as_f64()? * ratio * WINDOW_FRACTION;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(Size {
        width: (width.max(2.0)) as u32,
        height: (height.max(2.0)) as u32,
    })
}

#[component]
pub fn Desktop() -> impl IntoView {
    let status = RwSignal::new(String::from("connecting…"));
    let latency = Rc::new(Latency::new());
    let scene = Scene::new(latency.clone());
    let transport = connect(status, scene.clone(), latency.clone());

    let windows = scene.windows;
    let ids = Memo::new(move |_| windows.get().iter().map(|w| w.id).collect::<Vec<_>>());
    let desktop_ref: NodeRef<Main> = NodeRef::new();
    // Pointer and keyboard forwarding lives on the desktop element and routes by
    // event target, so a window that opens later needs no wiring of its own.
    // The window chrome handles raising, focus, dragging and closing itself.
    {
        let transport = transport.clone();
        let latency = latency.clone();
        Effect::new(move |_| {
            if let Some(desktop) = desktop_ref.get()
                && let Some(transport) = transport.clone()
            {
                crate::input::wire(&desktop, transport, latency.clone());
            }
        });
    }
    // `Rc` and the web-sys types behind it are not `Send`, which Leptos's
    // reactive graph requires of anything a view closure captures. Local storage
    // is the escape hatch for exactly this, and it makes the handles `Copy`.
    let scene = StoredValue::new_local(scene);
    let transport = StoredValue::new_local(transport);
    view! {
        <main node_ref=desktop_ref id="webland-desktop">
            <p class="status">{move || status.get()}</p>
            // Iterate ids, not states. `<For>` rebuilds a row whenever its item
            // value changes, and rebuilding a row means a brand new <canvas> —
            // leaving the renderer drawing into the detached one it captured,
            // which succeeds and shows nothing. An id never changes, so the row
            // is built once and everything inside it updates reactively.
            <For each=move || ids.get() key=|id| *id let:id>
                <Window id=id scene=scene transport=transport />
            </For>
            <Panel scene=scene transport=transport />
        </main>
    }
}

/// The panel: one button per open window, and a clock.
///
/// With windows stacked on top of each other a buried one is unreachable, so
/// this is what makes more than two of them usable at all. Raising from here is
/// browser state like any other stacking change; only the focus that comes with
/// it reaches the compositor.
#[component]
fn Panel(
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let windows = scene.with_value(|scene| scene.windows);
    let clock = RwSignal::new(now());
    // A minute is the resolution shown, so that is the resolution ticked.
    {
        let listener = Closure::<dyn FnMut()>::new(move || clock.set(now()));
        if let Some(window) = web_sys::window() {
            let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(
                listener.as_ref().unchecked_ref(),
                10_000,
            );
        }
        listener.forget();
    }

    let raise = move |id: u64| {
        scene.with_value(|scene| scene.raise(SurfaceId(id)));
        if let Some(transport) = transport.get_value()
            && let Ok(frame) = encode(&ClientMessage::Focus { id: SurfaceId(id) })
        {
            transport.send(&frame);
        }
    };

    view! {
        <footer id="webland-panel">
            <div class="tasks">
                <For
                    each=move || windows.get()
                    key=|window| (window.id, window.title.clone())
                    let:window
                >
                    <button class="task" on:pointerdown=move |_| raise(window.id)>
                        {window.title.clone()}
                    </button>
                </For>
            </div>
            <span class="clock">{move || clock.get()}</span>
        </footer>
    }
}

/// Wall clock as `HH:MM`, for the panel.
fn now() -> String {
    let date = js_sys::Date::new_0();
    format!("{:02}:{:02}", date.get_hours(), date.get_minutes())
}

/// One window: chrome the browser owns, wrapped around a canvas the compositor
/// fills.
#[component]
fn Window(
    id: u64,
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let send = move |message: &ClientMessage| {
        if let Some(transport) = transport.get_value()
            && let Ok(frame) = encode(message)
        {
            transport.send(&frame);
        }
    };
    let canvas_ref: NodeRef<Canvas> = NodeRef::new();

    // Attach a renderer once Leptos has actually put the canvas in the DOM, and
    // ask for a keyframe: anything sent before this had nowhere to land.
    Effect::new(move |_| {
        if let Some(canvas) = canvas_ref.get()
            && scene.with_value(|scene| scene.attach(id, &canvas))
        {
            send(&ClientMessage::RequestKeyframe);
        }
    });

    // Dragging by the title bar. Held here rather than in the scene because it
    // is per-window and lasts exactly as long as the gesture.
    let grab: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));

    let grab = StoredValue::new_local(grab);

    let start_drag = move |event: PointerEvent| {
        scene.with_value(|scene| {
            scene.raise(SurfaceId(id));
            let (x, y) = window_origin(scene, id);
            grab.with_value(|grab| {
                grab.set(Some((
                    event.client_x() - f64::from(x),
                    event.client_y() - f64::from(y),
                )));
            });
        });
    };
    let do_drag = move |event: PointerEvent| {
        let held = grab.with_value(|grab| grab.get());
        if let Some((dx, dy)) = held {
            #[allow(clippy::cast_possible_truncation)]
            scene.with_value(|scene| {
                scene.move_to(
                    id,
                    (event.client_x() - dx) as i32,
                    (event.client_y() - dy) as i32,
                );
            });
        }
    };
    let end_drag = move |_: PointerEvent| grab.with_value(|grab| grab.set(None));

    let close = move |_: PointerEvent| send(&ClientMessage::CloseSurface { id: SurfaceId(id) });

    // Raising and focusing are one gesture: the browser stacks, the compositor
    // only learns who has the seat.
    let focus = move |_: PointerEvent| {
        scene.with_value(|scene| scene.raise(SurfaceId(id)));
        send(&ClientMessage::Focus { id: SurfaceId(id) });
    };

    let frame_ref: NodeRef<Div> = NodeRef::new();
    // Read through the signal rather than the prop. `<For>` is keyed by id, so a
    // window whose title or position changes is not rebuilt — the prop is a
    // snapshot from the moment it first appeared, and a static style attribute
    // would leave dragging visibly doing nothing.
    let state = move || {
        scene.with_value(|scene| {
            scene
                .windows
                .with(|ws| ws.iter().find(|w| w.id == id).cloned())
        })
    };
    let style = move || {
        state().map_or_else(String::new, |w| {
            let ratio = crate::scene::pixel_ratio();
            format!(
                "left:{}px; top:{}px; z-index:{}; width:{}px;",
                w.x,
                w.y,
                w.z,
                f64::from(w.width) / ratio,
            )
        })
    };
    let title = move || state().map(|w| w.title).unwrap_or_default();
    // Memoised, and deliberately: assigning `canvas.width` clears the canvas
    // even when the value is unchanged, so a plain closure would wipe the
    // surface every time the window was raised or dragged — and an idle client
    // sends no frame to paint it back.
    let size = Memo::new(move |_| state().map_or((0, 0), |w| (w.width, w.height)));
    view! {
        <div node_ref=frame_ref class="window" style=style data-window=id.to_string()>
            <div class="titlebar" on:pointerdown=start_drag on:pointermove=do_drag
                 on:pointerup=end_drag on:pointercancel=end_drag>
                <span class="title">{title}</span>
                <button class="close" on:pointerdown=close title="Close">"×"</button>
            </div>
            // One bitmap pixel per *device* pixel. Dividing by the ratio is
            // what keeps a surface sharp: without it the browser stretches the
            // bitmap over `devicePixelRatio` screen pixels and softens it.
            <canvas node_ref=canvas_ref data-surface=id.to_string()
                    style=move || {
                        let ratio = crate::scene::pixel_ratio();
                        let (width, height) = size.get();
                        format!(
                            "width:{}px; height:{}px;",
                            f64::from(width) / ratio,
                            f64::from(height) / ratio,
                        )
                    }
                    on:pointerdown=focus></canvas>
        </div>
    }
}

/// A window's current top-left corner, for drag arithmetic.
fn window_origin(scene: &Scene, id: u64) -> (i32, i32) {
    scene
        .windows
        .with_untracked(|ws| ws.iter().find(|w| w.id == id).map(|w| (w.x, w.y)))
        .unwrap_or((0, 0))
}

/// Open the transport and wire it to the scene.
fn connect(
    status: RwSignal<String>,
    scene: Scene,
    latency: Rc<Latency>,
) -> Option<Rc<WebSocketTransport>> {
    let backend = backend();
    let transport = match WebSocketTransport::connect(&backend) {
        Ok(transport) => Rc::new(transport),
        Err(_) => {
            status.set(format!("could not open {backend}"));
            return None;
        }
    };

    // Frames carry only what changed, so a mid-stream joiner needs one full
    // surface to patch into.
    let opened = transport.clone();
    transport.on_open(Box::new(move || {
        status.set(String::from("connected — waiting for a surface…"));
        // Say how big the display is before asking for anything to put on it,
        // so the first frame arrives at the right size rather than at a guess.
        if let Some(size) = window_size()
            && let Ok(frame) = encode(&ClientMessage::Resize { size })
        {
            opened.send(&frame);
        }
        if let Ok(frame) = encode(&ClientMessage::RequestKeyframe) {
            opened.send(&frame);
        }
    }));

    // Follow the window: the browser is the display, so its size is the screen
    // resolution and a resized window is a mode change.
    {
        let resizing = transport.clone();
        let listener = Closure::<dyn FnMut()>::new(move || {
            if let Some(size) = window_size()
                && let Ok(frame) = encode(&ClientMessage::Resize { size })
            {
                resizing.send(&frame);
            }
        });
        if let Some(window) = web_sys::window() {
            let _ = window
                .add_event_listener_with_callback("resize", listener.as_ref().unchecked_ref());
        }
        listener.forget();
    }
    transport.on_close(Box::new(move || {
        status.set(String::from("disconnected — is the backend running?"));
    }));

    let ack = transport.clone();
    transport.on_message(Box::new(move |bytes| {
        let Ok(message) = decode::<ServerMessage>(&bytes) else {
            return;
        };
        // Acks name their surface so the compositor paces each one on its own.
        let presented = match &message {
            ServerMessage::SurfaceFrame(frame) => Some(frame.id),
            _ => None,
        };
        scene.handle(message);
        if let Some(id) = presented {
            status.set(latency.summary().unwrap_or_default());
            if let Ok(frame) = encode(&ClientMessage::FramePresented { id }) {
                ack.send(&frame);
            }
        }
    }));

    Some(transport)
}
