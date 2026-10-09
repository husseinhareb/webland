//! The shell's own titlebar: the window's name, its three buttons, and the drag
//! that moves it.
//!
//! A window that drew its own titlebar gets the `bare` class instead, and none of
//! this; its buttons reach the shell as `WindowRequest`s. See [`super::window`].

use std::rc::Rc;

use leptos::prelude::*;
use web_sys::PointerEvent;
use webland_core::SurfaceId;
use webland_protocol::{ClientMessage, encode};

use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

use super::style;
use super::window::capture;

#[component]
pub fn Titlebar(
    id: u64,
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let toggle_maximize = move || {
        if let Some(transport) = transport.get_value() {
            scene.with_value(|s| s.toggle_maximize(id, &transport));
        }
    };
    let start_drag = move |event: PointerEvent| {
        if let Some(transport) = transport.get_value() {
            scene.with_value(|s| {
                s.start_titlebar_drag(id, event.client_x(), event.client_y(), &transport);
            });
        }
        capture(&event);
    };
    let do_drag = move |event: PointerEvent| {
        if let Some(transport) = transport.get_value() {
            scene.with_value(|s| {
                s.update_titlebar_drag(event.client_x(), event.client_y(), &transport);
            });
        }
    };
    let end_drag = move |event: PointerEvent| {
        if let Some(transport) = transport.get_value() {
            scene.with_value(|s| {
                s.finish_titlebar_drag(event.client_x(), event.client_y(), &transport);
            });
        }
    };
    let open_menu = move |event: web_sys::MouseEvent| {
        event.prevent_default();
        event.stop_propagation();
        scene.with_value(|s| {
            s.titlebar_menu
                .set(Some((id, event.client_x(), event.client_y())));
        });
    };
    let minimize = move |event: PointerEvent| {
        event.stop_propagation();
        scene.with_value(|scene| scene.set_minimized(id, true));
    };
    let maximize = move |event: PointerEvent| {
        event.stop_propagation();
        toggle_maximize();
    };
    let close = move |event: PointerEvent| {
        event.stop_propagation();
        if let Some(transport) = transport.get_value()
            && let Ok(frame) = encode(&ClientMessage::CloseSurface { id: SurfaceId(id) })
        {
            transport.send(&frame);
        }
    };

    view! {
        <div class="titlebar" on:pointerdown=start_drag on:pointermove=do_drag
             on:pointerup=end_drag on:pointercancel=end_drag
             on:dblclick=move |_| toggle_maximize()
             on:contextmenu=open_menu>
            <span class="title">{move || style::title(scene, id)}</span>
            {move || {
                style::state(scene, id).filter(|w| w.pointer_locked).map(|_| {
                    view! {
                        <span class="lock-indicator"
                              title="Mouse captured (Click window to capture, ESC to release)">
                            <i class="fa-solid fa-lock"></i>
                            " Locked"
                        </span>
                    }
                })
            }}
            <button class="minimize" on:pointerdown=minimize title="Minimize">
                <i class="fa-solid fa-minus"></i>
            </button>
            <button class="maximize" on:pointerdown=maximize title="Maximize">
                <i class=move || {
                    let is_max = scene.with_value(|s| s.is_snapped(id) || s.is_maximized(id));
                    if is_max { "fa-regular fa-clone" } else { "fa-regular fa-square" }
                }></i>
            </button>
            <button class="close" on:pointerdown=close title="Close">
                <i class="fa-solid fa-xmark"></i>
            </button>
        </div>
    }
}
