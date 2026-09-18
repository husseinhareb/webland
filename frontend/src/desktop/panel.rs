//! The panel: one button per open window on this workspace, the launcher, the
//! workspace switcher, the capture and fullscreen toggles, and a clock.
//!
//! With windows stacked on top of each other a buried one is unreachable, so the
//! task buttons are what make more than two of them usable at all. Raising from
//! here is browser state like any other stacking change; only the focus that
//! comes with it reaches the compositor.

use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

use super::capture;
use super::launcher::Launcher;
use super::tasks::Tasks;

/// How many workspaces there are. Fixed at the usual four: a count nobody
/// changes is not a setting, and empty ones cost nothing.
pub const WORKSPACES: u32 = 4;

#[component]
pub fn Panel(
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let current = scene.with_value(|scene| scene.workspace);
    let captured = scene.with_value(|scene| scene.captured);
    let fullscreen = scene.with_value(|scene| scene.fullscreen);
    let open = RwSignal::new(false);
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

    let switch_workspace = move |n: u32| {
        current.set(n);
        scene.with_value(|s| s.show_toast(format!("Workspace {}", n + 1), None));
    };

    view! {
        {move || {
            open.get().then(|| {
                view! { <div class="launcher-backdrop" on:pointerdown=move |_| open.set(false) /> }
            })
        }}
        <footer id="webland-panel">
            <button
                class="launch"
                class:active=move || open.get()
                on:pointerdown=move |_| open.update(|open| *open = !*open)
            >
                "Apps"
            </button>
            <Launcher open=open scene=scene transport=transport />
            <div class="workspaces">
                <For each=move || 0..WORKSPACES key=|n| *n let:n>
                    <button
                        class="workspace"
                        class:here=move || current.get() == n
                        data-workspace=n.to_string()
                        title="Workspace — drop a window here to send it"
                        on:pointerdown=move |_| switch_workspace(n)
                    >
                        {(n + 1).to_string()}
                    </button>
                </For>
            </div>
            <Tasks scene=scene transport=transport />
            <div class="panel-controls">
                <button
                    class="panel-btn"
                    class:active=move || captured.get()
                    on:pointerdown=move |_| capture::toggle(scene)
                    title="Lock cursor inside desktop (ESC to release)"
                >
                    {move || if captured.get() { "Cursor Locked" } else { "Cursor Lock" }}
                </button>
                <button
                    class="panel-btn"
                    class:active=move || fullscreen.get()
                    on:pointerdown=move |_| capture::toggle_fullscreen(scene)
                    title="Toggle Fullscreen"
                >
                    {move || if fullscreen.get() { "🗗 Windowed" } else { "⛶ Fullscreen" }}
                </button>
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
