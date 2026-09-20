//! The system tray: the icons applications publish for themselves.
//!
//! These are not windows and have no surface. An application publishes a
//! `StatusNotifierItem` on the session bus, the server watches that bus and
//! sends over a picture and a label, and this draws them at the end of the
//! panel. A click goes back as the item's own action; the right button asks for
//! its menu, which is fetched when it is opened rather than kept; a tray menu
//! says what an application is doing *now*.
//!
//! Nothing here knows what an item is. The `id` is the server's handle for it
//! and travels back untouched.

use std::rc::Rc;

use leptos::prelude::*;
use web_sys::{MouseEvent, PointerEvent};
use webland_protocol::{ClientMessage, TrayMenuItem, encode};

use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

#[component]
pub fn SystemTray(
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let items = scene.with_value(|scene| scene.tray);
    let menu = scene.with_value(|scene| scene.tray_menu);

    let send = move |message: &ClientMessage| {
        if let Some(transport) = transport.get_value()
            && let Ok(frame) = encode(message)
        {
            transport.send(&frame);
        }
    };

    // One menu at a time, and a second click on the same icon closes it; a
    // tray icon is a toggle, not a one-way door.
    let open_menu = move |id: String| {
        if menu.get_untracked().is_some_and(|(open, _)| open == id) {
            menu.set(None);
        } else {
            menu.set(None);
            send(&ClientMessage::TrayMenuOpen { id });
        }
    };

    view! {
        <div class="systray">
            {move || {
                menu.get().map(|_| {
                    view! {
                        <div class="systray-backdrop" on:pointerdown=move |_| menu.set(None) />
                    }
                })
            }}
            <For each=move || items.get() key=|item| item.id.clone() let:item>
                {
                    let id = item.id.clone();
                    let menu_id = item.id.clone();
                    let clicked = item.id.clone();
                    let picture = icon(item.icon.clone(), &item.title);
                    view! {
                        <div class="systray-slot">
                            <button
                                class="systray-item"
                                class:open=move || {
                                    menu.get().is_some_and(|(open, _)| open == id)
                                }
                                title=item.title.clone()
                                on:pointerdown=move |event: PointerEvent| {
                                    match event.button() {
                                        // Left: the item's own action. Right and
                                        // middle are what a tray has always
                                        // meant by them.
                                        0 => send(&ClientMessage::TrayActivate {
                                            id: clicked.clone(),
                                            secondary: false,
                                        }),
                                        1 => send(&ClientMessage::TrayActivate {
                                            id: clicked.clone(),
                                            secondary: true,
                                        }),
                                        _ => open_menu(clicked.clone()),
                                    }
                                }
                                // Without this the browser's own menu opens over
                                // the application's.
                                on:contextmenu=move |event: MouseEvent| event.prevent_default()
                            >
                                {picture}
                            </button>
                            {move || {
                                menu.get()
                                    .filter(|(open, _)| *open == menu_id)
                                    .map(|(id, rows)| {
                                        view! { <Menu id=id rows=rows scene=scene transport=transport /> }
                                    })
                            }}
                        </div>
                    }
                }
            </For>
        </div>
    }
}

/// One item's menu, and its submenus.
#[component]
fn Menu(
    id: String,
    rows: Vec<TrayMenuItem>,
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    view! {
        <div class="systray-menu">
            <Rows id=id rows=rows scene=scene transport=transport />
        </div>
    }
}

/// The rows of a menu or submenu.
///
/// Recursive, because a tray menu is: the whole tree arrives in one answer, so
/// a submenu needs no round trip of its own and opens on hover like any other.
#[component]
fn Rows(
    id: String,
    rows: Vec<TrayMenuItem>,
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    rows.into_iter()
        .map(|row| {
            if row.separator {
                return view! { <div class="systray-separator" /> }.into_any();
            }
            let id = id.clone();
            let has_children = !row.children.is_empty();
            let children = row.children.clone();
            let label = row.label.clone();
            let enabled = row.enabled;
            let item = row.id;
            let click = move |_| {
                if !enabled {
                    return;
                }
                if let Some(transport) = transport.get_value()
                    && let Ok(frame) = encode(&ClientMessage::TrayMenuClick {
                        id: id.clone(),
                        item,
                    })
                {
                    transport.send(&frame);
                }
                // The application acts on the click; the menu's job is done, and
                // leaving it up would show state that is already stale.
                scene.with_value(|scene| scene.tray_menu.set(None));
            };
            if has_children {
                let id = row.id.to_string();
                return view! {
                    <div class="systray-row submenu">
                        <span class="systray-label">{label}</span>
                        <span class="systray-arrow">"›"</span>
                        <div class="systray-menu nested" data-row=id>
                            <Rows id=String::new() rows=children scene=scene transport=transport />
                        </div>
                    </div>
                }
                .into_any();
            }
            view! {
                <button class="systray-row" class:disabled=!enabled on:pointerdown=click>
                    <span class="systray-tick">
                        {row.checked.map_or(" ", |checked| if checked { "✓" } else { " " })}
                    </span>
                    <span class="systray-label">{label}</span>
                </button>
            }
            .into_any()
        })
        .collect_view()
}

/// The item's picture, or its initial when it published none.
fn icon(icon: Option<String>, title: &str) -> AnyView {
    icon.map_or_else(
        || {
            let initial = title.chars().next().map_or_else(
                || String::from("?"),
                |first| first.to_uppercase().to_string(),
            );
            view! { <span class="systray-initial">{initial}</span> }.into_any()
        },
        |icon| view! { <img class="systray-icon" src=icon alt="" /> }.into_any(),
    )
}
