//! The panel's task buttons: one per window on this workspace.
//!
//! With windows stacked on top of each other a buried one is unreachable, so
//! these are what make more than two of them usable at all. Raising from here is
//! browser state like any other stacking change; only the focus that comes with
//! it reaches the compositor.

use std::rc::Rc;

use leptos::prelude::*;
use webland_core::SurfaceId;
use webland_protocol::{ClientMessage, encode};

use crate::input::present_and_focus;
use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

#[component]
pub fn Tasks(
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let windows = scene.with_value(|scene| scene.windows);
    let current = scene.with_value(|scene| scene.workspace);

    let click = move |id: u64| {
        scene.with_value(|s| s.launcher_open.set(false));
        let focused = scene.with_value(|s| s.focused.get() == Some(id));
        let minimized = scene.with_value(|s| {
            s.windows
                .with(|ws| ws.iter().find(|w| w.id == id).is_some_and(|w| w.minimized))
        });
        if focused && !minimized {
            // Clicking the active window hides it, and the one below it takes
            // the seat: a task button that does nothing on a second click looks
            // broken.
            scene.with_value(|s| s.set_minimized(id, true));
            let next = next_below(scene, current.get(), id);
            scene.with_value(|s| s.focused.set(next));
            if let Some(next) = next
                && let Some(transport) = transport.get_value()
                && let Ok(frame) = encode(&ClientMessage::Focus {
                    id: SurfaceId(next),
                })
            {
                transport.send(&frame);
            }
            return;
        }
        scene.with_value(|scene| {
            // The panel un-hides as well as raises.
            scene.set_minimized(id, false);
            scene.raise(SurfaceId(id));
        });
        if let Some(transport) = transport.get_value() {
            present_and_focus(&transport, id);
        }
    };

    view! {
        <div class="tasks">
            // Only this workspace's windows. A task list showing every window on
            // every workspace is the thing workspaces exist to stop — and a menu
            // is not a window, so popups stay out of it whichever workspace they
            // are on.
            <For
                each=move || {
                    windows
                        .get()
                        .into_iter()
                        .filter(|w| w.parent.is_none() && w.workspace == current.get())
                        .collect::<Vec<_>>()
                }
                key=|window| (window.id, window.title.clone())
                let:window
            >
                <button
                    class="task"
                    class:active=move || scene.with_value(|s| s.focused.get() == Some(window.id))
                    on:pointerdown=move |_| click(window.id)
                >
                    {window.title.clone()}
                </button>
            </For>
        </div>
    }
}

/// The topmost window on `workspace` other than `skip`, if there is one.
fn next_below(scene: StoredValue<Scene, LocalStorage>, workspace: u32, skip: u64) -> Option<u64> {
    scene.with_value(|s| {
        s.windows.with(|ws| {
            ws.iter()
                .filter(|w| w.workspace == workspace && !w.minimized && w.id != skip)
                .max_by_key(|w| w.z)
                .map(|w| w.id)
        })
    })
}
