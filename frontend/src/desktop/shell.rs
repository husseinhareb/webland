//! The desktop shell: the root component, and the pointer-lock, fullscreen and
//! client-driven-move plumbing that only the root element sees.

use std::rc::Rc;

use leptos::html::Main;
use leptos::prelude::*;
use web_sys::PointerEvent;
use webland_core::{Size, SurfaceId};
use webland_protocol::{ClientMessage, encode};

use crate::latency::Latency;
use crate::scene::{Scene, SnapZone, pixel_ratio, pixels};

use super::chrome::{AltTabModal, ToastContainer};
use super::connect::connect;
use super::panel::Panel;
use super::window::Window;
use super::{capture, drag, wallpaper};

/// How much of the desktop a newly opened window takes up.
const WINDOW_FRACTION: f64 = 0.62;

/// The size to configure client windows at, in device pixels.
///
/// Device pixels, not CSS pixels: on a `HiDPI` screen each CSS pixel covers
/// `devicePixelRatio` real ones, so a surface rendered in CSS pixels gets
/// stretched to fit and looks soft. Rendering at the real count is what keeps it
/// sharp.
///
/// A fraction of the browser window rather than all of it, because these are
/// windows on a desktop now — at full size every one would cover the shell and
/// each other, and there would be nothing to drag.
pub fn window_size() -> Option<Size> {
    let window = web_sys::window()?;
    let ratio = pixel_ratio();
    let width = window.inner_width().ok()?.as_f64()? * ratio * WINDOW_FRACTION;
    let height = window.inner_height().ok()?.as_f64()? * ratio * WINDOW_FRACTION;
    Some(Size {
        width: pixels(width),
        height: pixels(height),
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
        let scene_wire = scene.clone();
        Effect::new(move |_| {
            if let Some(desktop) = desktop_ref.get()
                && let Some(transport) = transport.clone()
            {
                crate::input::wire(&desktop, &scene_wire, &transport, &latency);
            }
        });
    }
    // `Rc` and the web-sys types behind it are not `Send`, which Leptos's
    // reactive graph requires of anything a view closure captures. Local storage
    // is the escape hatch for exactly this, and it makes the handles `Copy`.
    let scene = StoredValue::new_local(scene);
    let transport = StoredValue::new_local(transport);

    // Whatever this browser last chose, back on the desktop before the first
    // window arrives.
    scene.with_value(|scene| scene.wallpaper.set(wallpaper::load()));
    let wallpaper = scene.with_value(|scene| scene.wallpaper);

    let captured = scene.with_value(|scene| scene.captured);
    let virtual_cursor = scene.with_value(|scene| scene.virtual_cursor);
    let cursor_icon = scene.with_value(|scene| scene.cursor_icon);
    capture::watch(scene);

    // When switching workspaces, proactively wake up visible windows on the new workspace
    // by acknowledging their latest frame, immediately stepping the compositor's FrameClock
    // out of the throttled idle pace.
    Effect::new(move |_| {
        let current_ws = scene.with_value(|s| s.workspace.get());
        if let Some(transport) = transport.get_value() {
            scene.with_value(|s| {
                // Untracked: the workspace is what this effect watches. Reading
                // the window list reactively made it re-run on every pixel of
                // every drag, acking a frame for every visible window at 60Hz
                // down a socket that had nothing to say.
                s.windows.with_untracked(|windows| {
                    for w in windows {
                        if w.parent.is_none()
                            && w.workspace == current_ws
                            && !w.minimized
                            && let Ok(frame) = encode(&ClientMessage::FramePresented {
                                id: SurfaceId(w.id),
                            })
                        {
                            transport.send(&frame);
                        }
                    }
                });
            });
        }
    });

    let drag = drag::ClientDrag::new(scene, transport);

    view! {
        <main node_ref=desktop_ref id="webland-desktop"
              style=move || wallpaper::style(wallpaper.get().as_ref())
              on:pointermove=move |event: PointerEvent| drag.moved(&event)
              on:pointerup=move |_| drag.dropped()
              on:pointercancel=move |_| drag.dropped()>
            {move || {
                captured.get().then(|| {
                    view! {
                        <div class="capture-banner">
                            <span class="capture-text">"Cursor Locked — Press ESC to release"</span>
                            <button class="capture-release-btn"
                                    on:pointerdown=move |_| capture::release(scene)>"Release"</button>
                        </div>
                    }
                })
            }}
            <div
                class=move || format!("virtual-cursor {}", cursor_icon.get())
                style=move || cursor_style(scene, captured.get(), virtual_cursor.get())
            />
            <p class="status" style=move || if captured.get() { "display: none;" } else { "" }>
                {move || status.get()}
            </p>
            {move || {
                scene.with_value(|s| {
                    s.snap_preview.get().map(|zone| {
                        let class = match zone {
                            SnapZone::Maximize => "snap-preview maximize",
                            SnapZone::Left => "snap-preview left",
                            SnapZone::Right => "snap-preview right",
                        };
                        view! { <div class=class /> }
                    })
                })
            }}
            // Iterate ids, not states. `<For>` rebuilds a row whenever its item
            // value changes, and rebuilding a row means a brand new <canvas> —
            // leaving the renderer drawing into the detached one it captured,
            // which succeeds and shows nothing. An id never changes, so the row
            // is built once and everything inside it updates reactively.
            <For each=move || ids.get() key=|id| *id let:id>
                <Window id=id scene=scene transport=transport />
            </For>
            <Panel scene=scene transport=transport />
            <AltTabModal scene=scene transport=transport />
            <ToastContainer scene=scene />
        </main>
    }
}

/// Where the virtual cursor is drawn, and whether it is drawn at all: not while
/// the real pointer is loose, and not while a client holds it for its own camera
/// — that client draws its own crosshair.
fn cursor_style(
    scene: StoredValue<Scene, LocalStorage>,
    captured: bool,
    at: Option<(f64, f64)>,
) -> String {
    let grabbed = scene.with_value(|scene| {
        scene
            .windows
            .with(|ws| ws.iter().any(|w| w.pointer_locked && !w.minimized))
    });
    if !captured || grabbed {
        return String::from("display: none;");
    }
    let (cx, cy) = at.unwrap_or_else(|| {
        let (w, h) = crate::input::viewport();
        (w / 2.0, h / 2.0)
    });
    format!("transform: translate3d({cx:.1}px, {cy:.1}px, 0); display: block;")
}
