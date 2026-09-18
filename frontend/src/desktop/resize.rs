//! One resize handle: a strip or corner along the window's edge that drags the
//! window's box ahead of the client.

use std::rc::Rc;

use leptos::prelude::*;
use web_sys::PointerEvent;

use crate::protocol::WebSocketTransport;
use crate::scene::{ResizeDirection, Scene};

use super::window::capture;

#[component]
pub fn ResizeHandle(
    id: u64,
    dir: ResizeDirection,
    class: &'static str,
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let start = move |event: PointerEvent| {
        if let Some(transport) = transport.get_value() {
            scene.with_value(|s| {
                s.start_active_resize(id, dir, event.client_x(), event.client_y(), &transport);
            });
        }
        capture(&event);
    };
    let update = move |event: PointerEvent| {
        scene.with_value(|s| s.update_active_resize(event.client_x(), event.client_y()));
    };
    let finish = move |_: PointerEvent| {
        if let Some(transport) = transport.get_value() {
            scene.with_value(|s| s.finish_active_resize(&transport));
        }
    };
    view! {
        <div class=format!("resize-handle {class}")
             on:pointerdown=start on:pointermove=update
             on:pointerup=finish on:pointercancel=finish></div>
    }
}
