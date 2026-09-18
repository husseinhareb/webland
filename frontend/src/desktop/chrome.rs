//! The two overlays that float above every window: the Alt+Tab switcher and
//! the toast stack.

use std::rc::Rc;

use leptos::prelude::*;
use webland_core::SurfaceId;
use webland_protocol::{ClientMessage, encode};

use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

/// The Alt+Tab window switcher modal: HUD showing open windows on the workspace.
#[component]
pub fn AltTabModal(
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let alt_tab = scene.with_value(|s| s.alt_tab);
    let windows = scene.with_value(|s| s.windows);

    let select_and_commit = move |id: u64| {
        scene.with_value(|s| {
            s.alt_tab.set(None);
            s.raise(SurfaceId(id));
        });
        if let Some(transport) = transport.get_value() {
            if let Ok(frame) = encode(&ClientMessage::FramePresented { id: SurfaceId(id) }) {
                transport.send(&frame);
            }
            if let Ok(frame) = encode(&ClientMessage::Focus { id: SurfaceId(id) }) {
                transport.send(&frame);
            }
        }
    };

    view! {
        {move || {
            alt_tab.get().map(|state| {
                let current_items = windows.with(|ws| {
                    state.window_ids.iter().filter_map(|&id| {
                        ws.iter().find(|w| w.id == id).map(|w| (w.id, w.title.clone()))
                    }).collect::<Vec<_>>()
                });
                view! {
                    <div class="alt-tab-backdrop">
                        <div class="alt-tab-container">
                            <For each=move || current_items.clone() key=|(id, _)| *id let:item>
                                {
                                    let item_id = item.0;
                                    let item_title = item.1.clone();
                                    let is_selected = move || {
                                        alt_tab.get().is_some_and(|s| {
                                            s.window_ids.get(s.selected_index) == Some(&item_id)
                                        })
                                    };
                                    view! {
                                        <button
                                            class="alt-tab-item"
                                            class:selected=is_selected
                                            on:pointerdown=move |_| select_and_commit(item_id)
                                        >
                                            <span class="alt-tab-icon">"🗖"</span>
                                            <span class="alt-tab-title">{item_title}</span>
                                        </button>
                                    }
                                }
                            </For>
                        </div>
                    </div>
                }
            })
        }}
    }
}

/// Floating system notifications in the top-right corner.
#[component]
pub fn ToastContainer(scene: StoredValue<Scene, LocalStorage>) -> impl IntoView {
    let toasts = scene.with_value(|s| s.toasts);
    let dismiss = move |id: u64| {
        toasts.update(|ts| ts.retain(|t| t.id != id));
    };

    view! {
        <div class="toast-container">
            <For each=move || toasts.get() key=|t| t.id let:toast>
                <div class="toast">
                    <div class="toast-body">
                        <div class="toast-title">{toast.title.clone()}</div>
                        {toast.message.clone().map(|msg| view! { <div class="toast-message">{msg}</div> })}
                    </div>
                    <button class="toast-close" on:pointerdown=move |_| dismiss(toast.id)>"×"</button>
                </div>
            </For>
        </div>
    }
}
