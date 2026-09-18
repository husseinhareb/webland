//! The titlebar's right-click menu: minimize, maximize, send to a workspace,
//! close.

use std::rc::Rc;

use leptos::prelude::*;
use web_sys::PointerEvent;
use webland_core::SurfaceId;
use webland_protocol::{ClientMessage, encode};

use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

use super::panel::WORKSPACES;

#[component]
pub fn TitlebarMenu(
    id: u64,
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let current = scene.with_value(|scene| scene.workspace);
    let close_menu = move || scene.with_value(|s| s.titlebar_menu.set(None));
    // Every item closes the menu and stops the press reaching the window
    // beneath it, so each handler starts the same way.
    let dismiss = move |event: &PointerEvent| {
        event.stop_propagation();
        close_menu();
    };
    let minimize = move |event: PointerEvent| {
        dismiss(&event);
        scene.with_value(|s| s.set_minimized(id, true));
    };
    let maximize = move |event: PointerEvent| {
        dismiss(&event);
        if let Some(transport) = transport.get_value() {
            scene.with_value(|s| s.toggle_maximize(id, &transport));
        }
    };
    let close = move |event: PointerEvent| {
        dismiss(&event);
        if let Some(transport) = transport.get_value()
            && let Ok(frame) = encode(&ClientMessage::CloseSurface { id: SurfaceId(id) })
        {
            transport.send(&frame);
        }
    };

    view! {
        {move || {
            scene.with_value(|s| {
                s.titlebar_menu.get().filter(|(target, _, _)| *target == id).map(|(_, mx, my)| {
                    let is_max = s.is_snapped(id) || s.is_maximized(id);
                    view! {
                        <div class="menu-backdrop" on:pointerdown=move |e: PointerEvent| dismiss(&e)>
                            <div class="titlebar-menu"
                                 style=format!("left:{mx}px; top:{my}px;")
                                 on:pointerdown=move |e: PointerEvent| e.stop_propagation()>
                                <button data-action="minimize"
                                        on:pointerdown=minimize>
                                    "– Minimize"
                                </button>
                                <button data-action="maximize"
                                        on:pointerdown=maximize>
                                    {if is_max { "❐ Restore" } else { "□ Maximize" }}
                                </button>
                                <div class="menu-separator"></div>
                                <div class="menu-section-label">"Workspace"</div>
                                <div class="menu-workspace-row">
                                    <For each=move || 0..WORKSPACES key=|n| *n let:n>
                                        <button
                                            class="menu-ws-btn"
                                            data-workspace=n.to_string()
                                            class:active=move || current.get() == n
                                            on:pointerdown=move |e: PointerEvent| {
                                                dismiss(&e);
                                                scene.with_value(|s| s.send_to_workspace(id, n));
                                            }
                                        >
                                            {(n + 1).to_string()}
                                        </button>
                                    </For>
                                </div>
                                <div class="menu-separator"></div>
                                <button class="menu-danger" data-action="close"
                                        on:pointerdown=close>
                                    "× Close"
                                </button>
                            </div>
                        </div>
                    }
                })
            })
        }}
    }
}
