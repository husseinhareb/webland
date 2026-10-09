//! The panel's settings: the chevron at the end of the bar, and everything the
//! viewer can change behind it.
//!
//! These are the settings that belong to the browser showing the desktop rather
//! than to any window in it (cursor lock, fullscreen, the wallpaper, the
//! keyboard in front of the viewer), and a desktop keeps those out of the bar
//! itself, one click away, the way Windows has since the chevron appeared
//! beside its clock. What is chosen here persists: see [`crate::scene::Settings`].

use leptos::html::Input;
use leptos::prelude::*;

use crate::scene::Scene;

use super::{capture, wallpaper};

/// Common xkb layouts, offered as suggestions; any other name can be typed.
const LAYOUTS: [(&str, &str); 12] = [
    ("us", "English (US)"),
    ("gb", "English (UK)"),
    ("fr", "French"),
    ("de", "German"),
    ("es", "Spanish"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("br", "Portuguese (Brazil)"),
    ("ru", "Russian"),
    ("ara", "Arabic"),
    ("tr", "Turkish"),
    ("jp", "Japanese"),
];

#[component]
pub fn Overflow(scene: StoredValue<Scene, LocalStorage>) -> impl IntoView {
    let captured = scene.with_value(|scene| scene.captured);
    let fullscreen = scene.with_value(|scene| scene.fullscreen);
    let settings = scene.with_value(|scene| scene.settings);
    let open = RwSignal::new(false);
    let wallpaper_ref: NodeRef<Input> = NodeRef::new();

    view! {
        <div class="panel-controls">
            {move || {
                open.get().then(|| {
                    view! {
                        <div class="overflow-backdrop"
                             on:pointerdown=move |_| open.set(false) />
                    }
                })
            }}
            <button
                class="panel-btn overflow-toggle"
                class:active=move || open.get()
                on:pointerdown=move |_| open.update(|open| *open = !*open)
                title="Settings"
            >
                <i class="fa-solid fa-chevron-up"></i>
            </button>
            <div class="overflow" class:open=move || open.get()>
                <div class="overflow-heading">"Session"</div>
                <button
                    class="overflow-item"
                    class:active=move || captured.get()
                    on:pointerdown=move |_| {
                        open.set(false);
                        capture::toggle(scene);
                    }
                    title="Lock cursor inside desktop (ESC to release)"
                >
                    <i class=move || if captured.get() { "fa-solid fa-lock" } else { "fa-solid fa-arrow-pointer" }></i>
                    {move || if captured.get() { " Cursor Locked" } else { " Cursor Lock" }}
                </button>
                <button
                    class="overflow-item"
                    class:active=move || fullscreen.get()
                    on:pointerdown=move |_| {
                        open.set(false);
                        capture::toggle_fullscreen(scene);
                    }
                    title="Toggle Fullscreen"
                >
                    <i class=move || if fullscreen.get() { "fa-solid fa-compress" } else { "fa-solid fa-expand" }></i>
                    {move || if fullscreen.get() { " Windowed" } else { " Fullscreen" }}
                </button>

                <div class="overflow-heading">"Appearance"</div>
                // The picker is the platform's own; the button is only
                // there because a bare file input looks like a form.
                <input
                    node_ref=wallpaper_ref
                    class="wallpaper-file"
                    type="file"
                    accept="image/*"
                    on:change=move |_| {
                        open.set(false);
                        if let Some(input) = wallpaper_ref.get() {
                            wallpaper::chosen(&input, scene);
                        }
                    }
                />
                <button
                    class="overflow-item"
                    on:pointerdown=move |_| {
                        if let Some(input) = wallpaper_ref.get() {
                            input.click();
                        }
                    }
                    title="Pick a background image"
                >
                    <i class="fa-regular fa-image"></i>
                    " Wallpaper…"
                </button>
                {move || {
                    scene.with_value(|s| s.wallpaper.get()).is_some().then(|| {
                        view! {
                            <button
                                class="overflow-item"
                                on:pointerdown=move |_| {
                                    open.set(false);
                                    wallpaper::clear(scene);
                                }
                            >
                                <i class="fa-solid fa-trash-can"></i>
                                " Clear Wallpaper"
                            </button>
                        }
                    })
                }}
                <label class="overflow-row">
                    "24-hour clock"
                    <input type="checkbox" prop:checked=settings.clock_24h
                        on:change=move |e| settings.clock_24h.set(event_target_checked(&e)) />
                </label>
                <label class="overflow-row">
                    "Network speed"
                    <input type="checkbox" prop:checked=settings.show_stats
                        on:change=move |e| settings.show_stats.set(event_target_checked(&e)) />
                </label>

                <div class="overflow-heading">"Notifications"</div>
                <label class="overflow-row">
                    "Do not disturb"
                    <input type="checkbox" prop:checked=settings.do_not_disturb
                        on:change=move |e| settings.do_not_disturb.set(event_target_checked(&e)) />
                </label>
                <label class="overflow-row">
                    "Show for (seconds)"
                    <input type="number" min="1" max="60" class="overflow-field narrow"
                        prop:value=move || settings.notification_seconds.get().to_string()
                        on:change=move |e| {
                            if let Ok(seconds) = event_target_value(&e).parse::<u32>() {
                                settings.notification_seconds.set(seconds.clamp(1, 60));
                            }
                        } />
                </label>

                <div class="overflow-heading">"Keyboard"</div>
                // Committed on change, not per keystroke: half a layout name is
                // not a layout, and xkb would refuse each one on the way.
                <label class="overflow-row">
                    "Layout"
                    <input type="text" list="webland-layouts" class="overflow-field"
                        placeholder="host default" spellcheck="false"
                        prop:value=settings.layout
                        on:change=move |e| settings.layout.set(event_target_value(&e).trim().to_owned()) />
                </label>
                <label class="overflow-row">
                    "Variant"
                    <input type="text" class="overflow-field"
                        placeholder="none" spellcheck="false"
                        prop:value=settings.variant
                        on:change=move |e| settings.variant.set(event_target_value(&e).trim().to_owned()) />
                </label>
                <datalist id="webland-layouts">
                    {LAYOUTS.map(|(code, name)| view! { <option value=code>{name}</option> })}
                </datalist>
            </div>
        </div>
    }
}
