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
                key=|window| (window.id, window.title.clone(), window.app_id.clone())
                let:window
            >
                <button
                    class="task"
                    class:active=move || scene.with_value(|s| s.focused.get() == Some(window.id))
                    on:pointerdown=move |_| click(window.id)
                    title=window.title.clone()
                >
                    {icon(scene, window.app_id.clone())}
                    <span class="task-title">{window.title.clone()}</span>
                </button>
            </For>
        </div>
    }
}

/// The application icon for a window, if its client named an application the
/// launcher also knows about.
///
/// The match is on the `.desktop` file's basename, which is what `app_id` is
/// supposed to be — and often is not exactly: a client may report `Navigator`
/// or trail a `.desktop`, so the comparison is case-insensitive and settles for
/// one name ending in the other rather than demanding they be equal.
fn icon(scene: StoredValue<Scene, LocalStorage>, app_id: Option<String>) -> impl IntoView {
    let applications = scene.with_value(|scene| scene.applications);
    move || {
        let app_id = app_id.as_ref()?.to_lowercase();
        let icon = applications.with(|apps| {
            apps.iter()
                .find(|app| matches(&app.app_id.to_lowercase(), &app_id))
                .and_then(|app| app.icon.clone())
        })?;
        Some(view! { <img class="task-icon" src=icon alt="" /> })
    }
}

/// Whether a client's `app_id` names the same application as a `.desktop`
/// file's basename.
fn matches(stem: &str, app_id: &str) -> bool {
    let app_id = app_id.strip_suffix(".desktop").unwrap_or(app_id);
    if stem == app_id || stem.rsplit('.').next() == app_id.rsplit('.').next() {
        return true;
    }
    // A suffix match is how `org.gnome.Nautilus` finds `nautilus`, but on two
    // or three letters it is not a match, it is a coincidence.
    stem.len().min(app_id.len()) >= 4 && (stem.ends_with(app_id) || app_id.ends_with(stem))
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
