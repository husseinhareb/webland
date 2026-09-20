//! The panel's overflow tray: the chevron at the end of the bar, and the
//! session's own controls behind it.
//!
//! These are the settings that belong to the browser showing the desktop rather
//! than to any window in it — cursor lock, fullscreen, the wallpaper — and a
//! desktop keeps those out of the bar itself, one click away, the way Windows
//! has since the chevron appeared beside its clock.

use leptos::html::Input;
use leptos::prelude::*;

use crate::scene::Scene;

use super::{capture, wallpaper};

#[component]
pub fn Tray(scene: StoredValue<Scene, LocalStorage>) -> impl IntoView {
    let captured = scene.with_value(|scene| scene.captured);
    let fullscreen = scene.with_value(|scene| scene.fullscreen);
    let tray_open = RwSignal::new(false);
    let wallpaper_ref: NodeRef<Input> = NodeRef::new();

    view! {
        <div class="panel-controls">
            {move || {
                tray_open.get().then(|| {
                    view! {
                        <div class="tray-backdrop"
                             on:pointerdown=move |_| tray_open.set(false) />
                    }
                })
            }}
            <button
                class="panel-btn tray-toggle"
                class:active=move || tray_open.get()
                on:pointerdown=move |_| tray_open.update(|tray| *tray = !*tray)
                title="Session controls"
            >
                "⌃"
            </button>
            <div class="tray" class:open=move || tray_open.get()>
                <button
                    class="tray-item"
                    class:active=move || captured.get()
                    on:pointerdown=move |_| {
                        tray_open.set(false);
                        capture::toggle(scene);
                    }
                    title="Lock cursor inside desktop (ESC to release)"
                >
                    {move || if captured.get() { "Cursor Locked" } else { "Cursor Lock" }}
                </button>
                <button
                    class="tray-item"
                    class:active=move || fullscreen.get()
                    on:pointerdown=move |_| {
                        tray_open.set(false);
                        capture::toggle_fullscreen(scene);
                    }
                    title="Toggle Fullscreen"
                >
                    {move || if fullscreen.get() { "🗗 Windowed" } else { "⛶ Fullscreen" }}
                </button>
                // The picker is the platform's own; the button is only
                // there because a bare file input looks like a form.
                <input
                    node_ref=wallpaper_ref
                    class="wallpaper-file"
                    type="file"
                    accept="image/*"
                    on:change=move |_| {
                        tray_open.set(false);
                        if let Some(input) = wallpaper_ref.get() {
                            wallpaper::chosen(&input, scene);
                        }
                    }
                />
                <button
                    class="tray-item"
                    on:pointerdown=move |_| {
                        if let Some(input) = wallpaper_ref.get() {
                            input.click();
                        }
                    }
                    title="Pick a background image"
                >
                    "Wallpaper…"
                </button>
                {move || {
                    scene.with_value(|s| s.wallpaper.get()).is_some().then(|| {
                        view! {
                            <button
                                class="tray-item"
                                on:pointerdown=move |_| {
                                    tray_open.set(false);
                                    wallpaper::clear(scene);
                                }
                            >
                                "Clear Wallpaper"
                            </button>
                        }
                    })
                }}
            </div>
        </div>
    }
}
