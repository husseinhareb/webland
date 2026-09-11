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
use webland_protocol::WindowRequest;

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

/// How many workspaces there are. Fixed at the usual four: a count nobody
/// changes is not a setting, and empty ones cost nothing.
const WORKSPACES: u32 = 4;

/// Where a resize began: the pointer, and the size the window had then. Deltas
/// are taken from the start rather than the last move, so rounding cannot
/// accumulate over a long drag.
type Stretch = (f64, f64, u32, u32);

/// Where a client-driven move has the pointer, relative to the window's corner.
/// Empty until the first pointer event of the gesture: the client asks to be
/// moved without saying from where.
type Held = StoredValue<Rc<Cell<Option<(f64, f64)>>>, LocalStorage>;

/// Where a move began: the pointer's offset into the titlebar, and the corner
/// the window started from — kept so a drag that ends on a workspace button can
/// put the window back rather than leave it parked over the panel.
type Grab = (f64, f64, i32, i32);

/// The smallest a window may be dragged, in device pixels. Small enough to be
/// no real limit, large enough that a window can never lose its own grip.
const MIN_SURFACE: f64 = 160.0;

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

/// The whole desktop, in device pixels: everything the panel has not taken.
///
/// Measured rather than assumed, so the panel's height lives in the stylesheet
/// alone and the two cannot drift apart.
fn maximized_size() -> Option<Size> {
    let window = web_sys::window()?;
    let ratio = crate::scene::pixel_ratio();
    let panel = window
        .document()
        .and_then(|document| document.query_selector("#webland-panel").ok().flatten())
        .map_or(0.0, |panel| f64::from(panel.client_height()));
    let width = window.inner_width().ok()?.as_f64()? * ratio;
    let height = (window.inner_height().ok()?.as_f64()? - panel) * ratio;
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

    // Moving a window whose client asked to be moved — a GTK titlebar drag.
    // It runs here rather than in the window, because the pointer is over the
    // client's own surface: no piece of shell chrome saw the gesture start, and
    // the desktop is the one element every later pointer event reaches.
    let dragging = scene.with_value(|scene| scene.dragging);
    let held: Held = StoredValue::new_local(Rc::new(Cell::new(None)));
    let drag_window = move |event: PointerEvent| {
        let Some(id) = dragging.get_untracked() else {
            return;
        };
        // The client says "move me", never from where, so the offset is taken
        // on the first pointer event that follows and kept for the gesture.
        let (dx, dy) = held.with_value(|held| held.get()).unwrap_or_else(|| {
            let (x, y) = scene.with_value(|scene| window_origin(scene, id));
            (
                event.client_x() - f64::from(x),
                event.client_y() - f64::from(y),
            )
        });
        held.with_value(|held| held.set(Some((dx, dy))));
        #[allow(clippy::cast_possible_truncation)]
        scene.with_value(|scene| {
            scene.move_to(
                id,
                (event.client_x() - dx) as i32,
                (event.client_y() - dy) as i32,
            );
        });
    };
    let drop_window = move |_: PointerEvent| {
        if dragging.get_untracked().is_some() {
            dragging.set(None);
            held.with_value(|held| held.set(None));
        }
    };

    view! {
        <main node_ref=desktop_ref id="webland-desktop"
              on:pointermove=drag_window on:pointerup=drop_window
              on:pointercancel=drop_window>
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
    let applications = scene.with_value(|scene| scene.applications);
    let current = scene.with_value(|scene| scene.workspace);
    let open = RwSignal::new(false);
    let filter = RwSignal::new(String::new());
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
        scene.with_value(|scene| {
            // The panel is the only way back from minimized, so its button
            // un-hides as well as raises.
            scene.set_minimized(id, false);
            scene.raise(SurfaceId(id));
        });
        if let Some(transport) = transport.get_value()
            && let Ok(frame) = encode(&ClientMessage::Focus { id: SurfaceId(id) })
        {
            transport.send(&frame);
        }
    };

    let launch = move |id: u32| {
        open.set(false);
        filter.set(String::new());
        if let Some(transport) = transport.get_value()
            && let Ok(frame) = encode(&ClientMessage::Launch { id })
        {
            transport.send(&frame);
        }
    };

    // Filtered by a plain substring match. A launcher people type two letters
    // into does not need fuzzy ranking to be useful, and the list is short.
    let matching = move || {
        let needle = filter.get().to_lowercase();
        applications
            .get()
            .into_iter()
            .filter(|app| needle.is_empty() || app.name.to_lowercase().contains(&needle))
            .take(40)
            .collect::<Vec<_>>()
    };

    view! {
        <footer id="webland-panel">
            <button
                class="launch"
                on:pointerdown=move |_| open.update(|open| *open = !*open)
            >
                "Apps"
            </button>
            <div class="menu" class:open=move || open.get()>
                <input
                    class="search"
                    placeholder="Search…"
                    prop:value=move || filter.get()
                    on:input=move |event| filter.set(event_value(&event))
                />
                <div class="results">
                    <For each=matching key=|app| app.id let:app>
                        <button class="app" on:pointerdown=move |_| launch(app.id)>
                            // An icon the theme did not have leaves a gap the
                            // width of one, so the names stay in a column.
                            <span class="icon">
                                {app.icon.clone().map(|icon| view! { <img src=icon alt="" /> })}
                            </span>
                            {app.name.clone()}
                        </button>
                    </For>
                </div>
            </div>
            <div class="workspaces">
                <For each=move || 0..WORKSPACES key=|n| *n let:n>
                    <button
                        class="workspace"
                        class:here=move || current.get() == n
                        data-workspace=n.to_string()
                        title="Workspace — drop a window here to send it"
                        on:pointerdown=move |_| current.set(n)
                    >
                        {(n + 1).to_string()}
                    </button>
                </For>
            </div>
            <div class="tasks">
                // Only this workspace's windows. A task list showing every
                // window on every workspace is the thing workspaces exist to
                // stop.
                <For
                    each=move || {
                        windows.get().into_iter().filter(|w| w.workspace == current.get()).collect::<Vec<_>>()
                    }
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

/// The current text of an `<input>` an event came from.
fn event_value(event: &web_sys::Event) -> String {
    event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
        .map(|input| input.value())
        .unwrap_or_default()
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
    let grab: Rc<Cell<Option<Grab>>> = Rc::new(Cell::new(None));

    let grab = StoredValue::new_local(grab);

    let start_drag = move |event: PointerEvent| {
        scene.with_value(|scene| {
            scene.raise(SurfaceId(id));
            let (x, y) = window_origin(scene, id);
            grab.with_value(|grab| {
                grab.set(Some((
                    event.client_x() - f64::from(x),
                    event.client_y() - f64::from(y),
                    x,
                    y,
                )));
            });
        });
        // Capture, so the bar keeps the gesture once the pointer leaves it: over
        // the panel, which sits above every window, and whenever a fast drag
        // outruns the window it is moving.
        capture(&event);
    };
    let do_drag = move |event: PointerEvent| {
        let held = grab.with_value(|grab| grab.get());
        if let Some((dx, dy, _, _)) = held {
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
    // Dropping a window on a workspace button sends it there. The gesture is
    // already in hand — this only asks what is under the pointer when it ends —
    // and dragging a window onto a workspace is the idiom every desktop uses,
    // so it needs no affordance of its own.
    let end_drag = move |event: PointerEvent| {
        let Some((_, _, from_x, from_y)) = grab.with_value(|grab| grab.take()) else {
            return;
        };
        if let Some(workspace) = workspace_under(&event) {
            scene.with_value(|scene| {
                // Back where it started. The drag was a gesture aimed at the
                // panel, not at a new position — dropping it where the pointer
                // happened to be leaves the window parked over the panel, mostly
                // off-screen, on a workspace the user is about to be shown.
                scene.move_to(id, from_x, from_y);
                scene.send_to_workspace(id, workspace);
            });
        }
    };

    // Resizing by the corner grip. Same shape as the drag: the anchor is where
    // the pointer was when the gesture began, held for as long as it lasts.
    let stretch: Rc<Cell<Option<Stretch>>> = Rc::new(Cell::new(None));
    let stretch = StoredValue::new_local(stretch);

    let start_resize = move |event: PointerEvent| {
        scene.with_value(|scene| {
            scene.raise(SurfaceId(id));
            let (width, height) = window_size_of(scene, id);
            stretch.with_value(|stretch| {
                stretch.set(Some((
                    event.client_x(),
                    event.client_y(),
                    width,
                    height,
                )));
            });
        });
        // The grip keeps receiving moves once the pointer leaves it, which for a
        // grip a few pixels wide is immediately.
        capture(&event);
    };
    let do_resize = move |event: PointerEvent| {
        let Some((from_x, from_y, width, height)) = stretch.with_value(|stretch| stretch.get()) else {
            return;
        };
        // The grip moves in CSS pixels; surfaces are measured in device pixels.
        let ratio = crate::scene::pixel_ratio();
        let dx = (event.client_x() - from_x) * ratio;
        let dy = (event.client_y() - from_y) * ratio;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (width, height) = (
            (f64::from(width) + dx).max(MIN_SURFACE) as u32,
            (f64::from(height) + dy).max(MIN_SURFACE) as u32,
        );
        scene.with_value(|scene| scene.resize_to(id, width, height));
    };
    let end_resize = move |_: PointerEvent| {
        if stretch.with_value(|stretch| stretch.take()).is_none() {
            return;
        }
        // Told once, at the end. Every configure costs the client a
        // reallocation and the wire a keyframe, so a drag that sent one per
        // pointermove would spend hundreds of them to answer one question.
        let (width, height) = scene.with_value(|scene| window_size_of(scene, id));
        send(&ClientMessage::SetSize {
            id: SurfaceId(id),
            size: Size { width, height },
        });
    };

    let close = move |_: PointerEvent| send(&ClientMessage::CloseSurface { id: SurfaceId(id) });

    let minimize = move |_: PointerEvent| scene.with_value(|scene| scene.set_minimized(id, true));

    let set_maximized = move |maximize: bool| {
        scene.with_value(|scene| scene.set_maximized(id, maximize));
        send(&ClientMessage::SetMaximized {
            id: SurfaceId(id),
            size: if maximize { maximized_size() } else { None },
        });
    };
    let toggle_maximize = move |_: PointerEvent| {
        set_maximized(!scene.with_value(|scene| scene.is_maximized(id)));
    };

    // The same gestures, arriving from the client's own titlebar instead of the
    // shell's. A self-decorated window has no chrome here to click, so this is
    // the only way its buttons do anything at all.
    Effect::new(move |_| {
        let Some((target, request)) = scene.with_value(|scene| scene.requests.get()) else {
            return;
        };
        if target != id {
            return;
        }
        scene.with_value(|scene| scene.requests.set(None));
        match request {
            WindowRequest::Move => scene.with_value(|scene| {
                scene.raise(SurfaceId(id));
                scene.dragging.set(Some(id));
            }),
            WindowRequest::Maximize => set_maximized(true),
            WindowRequest::Unmaximize => set_maximized(false),
            WindowRequest::Minimize => scene.with_value(|scene| scene.set_minimized(id, true)),
        }
    });

    // Raising and focusing are one gesture: the browser stacks, the compositor
    // only learns who has the seat.
    //
    // Alt held makes it a move instead, which is the only way to shift a window
    // that has no titlebar here and whose client does not offer one either — a
    // client that decorates itself is assumed to, but nothing makes it.
    let focus = move |event: PointerEvent| {
        scene.with_value(|scene| scene.raise(SurfaceId(id)));
        if event.alt_key() {
            scene.with_value(|scene| scene.dragging.set(Some(id)));
            return;
        }
        send(&ClientMessage::Focus { id: SurfaceId(id) });
    };

    let current = scene.with_value(|scene| scene.workspace);
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
            // Hidden with `display`, never unmounted: tearing the row down would
            // take the canvas with it and leave the renderer drawing into a
            // detached one, which succeeds and shows nothing ever after. Another
            // workspace hides a window exactly as minimizing does, and for the
            // same reason.
            //
            // ponytail: a hidden window goes on streaming, and off-screen
            // surfaces are the encode cost the roadmap's risk table flags. Tell
            // the compositor to stop sending them if window count starts to hurt.
            let elsewhere = w.workspace != current.get();
            let hidden = if w.minimized || elsewhere {
                "display:none;"
            } else {
                ""
            };
            format!(
                "left:{}px; top:{}px; z-index:{}; width:{}px; {hidden}",
                w.x,
                w.y,
                w.z,
                f64::from(w.width) / ratio,
            )
        })
    };
    let title = move || state().map(|w| w.title).unwrap_or_default();
    // The canvas is the whole streamed image, which for a client that drew a
    // shadow around itself is bigger than the window; the box it sits in is the
    // window, and the canvas is offset so the window's corner lands in the
    // corner. A resize drag scales both, stretching the last frame until the
    // client answers at the new size — the same stretch, applied to a picture
    // that is now only partly on show.
    let scale = move || {
        state().map_or((1.0, 1.0), |w| {
            (
                f64::from(w.width) / f64::from(w.content.width.max(1)),
                f64::from(w.height) / f64::from(w.content.height.max(1)),
            )
        })
    };
    let surface_style = move || {
        state().map_or_else(String::new, |w| {
            let ratio = crate::scene::pixel_ratio();
            format!(
                "width:{}px; height:{}px;",
                f64::from(w.width) / ratio,
                f64::from(w.height) / ratio,
            )
        })
    };
    let canvas_style = move || {
        state().map_or_else(String::new, |w| {
            let ratio = crate::scene::pixel_ratio();
            let (sx, sy) = scale();
            let (image_w, image_h) = w.image;
            format!(
                "width:{}px; height:{}px; left:{}px; top:{}px;",
                f64::from(image_w) * sx / ratio,
                f64::from(image_h) * sy / ratio,
                -f64::from(w.content.x) * sx / ratio,
                -f64::from(w.content.y) * sy / ratio,
            )
        })
    };
    view! {
        // `bare` drops the shell's titlebar for a window that drew its own.
        //
        // ponytail: it is dropped for anything that never asked to be decorated,
        // which xdg-decoration says to read as "the client decorates itself" —
        // true of GTK, and of nothing that draws no chrome at all. Such a window
        // keeps its resize grip and moves on alt-drag, but loses the shell's
        // close button; give the panel's task button a close if one turns up.
        <div node_ref=frame_ref class="window" style=style data-window=id.to_string()
             class:bare=move || state().is_some_and(|w| !w.decorated)>
            <div class="titlebar" on:pointerdown=start_drag on:pointermove=do_drag
                 on:pointerup=end_drag on:pointercancel=end_drag>
                <span class="title">{title}</span>
                <button class="minimize" on:pointerdown=minimize title="Minimize">"–"</button>
                <button class="maximize" on:pointerdown=toggle_maximize title="Maximize">"□"</button>
                <button class="close" on:pointerdown=close title="Close">"×"</button>
            </div>
            // One bitmap pixel per *device* pixel. Dividing by the ratio is
            // what keeps a surface sharp: without it the browser stretches the
            // bitmap over `devicePixelRatio` screen pixels and softens it.
            <div class="surface" style=surface_style>
                <canvas node_ref=canvas_ref data-surface=id.to_string()
                        style=canvas_style on:pointerdown=focus></canvas>
            </div>
            <div class="grip" on:pointerdown=start_resize on:pointermove=do_resize
                 on:pointerup=end_resize on:pointercancel=end_resize
                 title="Resize"></div>
        </div>
    }
}

/// The size a window is currently drawn at, for resize arithmetic.
fn window_size_of(scene: &Scene, id: u64) -> (u32, u32) {
    scene
        .windows
        .with_untracked(|ws| ws.iter().find(|w| w.id == id).map(|w| (w.width, w.height)))
        .unwrap_or((1, 1))
}

/// Give the element a gesture started on the rest of that gesture, wherever the
/// pointer goes.
fn capture(event: &PointerEvent) {
    if let Some(target) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    {
        let _ = target.set_pointer_capture(event.pointer_id());
    }
}

/// The workspace button under a pointer, if the gesture ended on one.
///
/// Read from the DOM rather than tracked, because the panel is the only thing
/// that knows where its own buttons are, and it draws them from a stylesheet.
fn workspace_under(event: &PointerEvent) -> Option<u32> {
    let document = web_sys::window()?.document()?;
    document
        .element_from_point(event.client_x() as f32, event.client_y() as f32)?
        .closest("[data-workspace]")
        .ok()
        .flatten()?
        .get_attribute("data-workspace")?
        .parse()
        .ok()
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
