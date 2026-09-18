//! The wheel. Without this the page scrolls under the desktop instead of the
//! application scrolling inside its window, which is the wrong thing in the most
//! confusing possible way: it looks like the click went somewhere.

use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{HtmlCanvasElement, WheelEvent};
use webland_core::SurfaceId;
use webland_protocol::InputEvent;

use crate::protocol::WebSocketTransport;
use crate::scene::{Scene, element_at};

use super::dom::{canvas_surface_id, target_canvas};
use super::wire::send;

/// Attach the wheel listener.
pub fn install(scene: &Scene, transport: &Rc<WebSocketTransport>) {
    {
        let transport = transport.clone();
        let scene = scene.clone();
        let listener = Closure::<dyn FnMut(WheelEvent)>::new(move |event: WheelEvent| {
            let doc = web_sys::window().and_then(|w| w.document());
            let is_locked = doc
                .as_ref()
                .and_then(web_sys::Document::pointer_lock_element)
                .is_some();

            if is_locked {
                if let Some((cx, cy)) = scene.virtual_cursor.get_untracked()
                    && let Some(el) = doc.as_ref().and_then(|d| element_at(d, cx, cy))
                    && let Ok(canvas) = el.dyn_into::<HtmlCanvasElement>()
                    && let Some(id) = canvas_surface_id(&canvas)
                {
                    event.prevent_default();
                    let (dx, dy) = wheel_pixels(&event);
                    send(
                        &transport,
                        InputEvent::PointerScroll {
                            id: SurfaceId(id),
                            dx,
                            dy,
                        },
                    );
                }
                return;
            }

            // The wheel turns over the window the cursor is on, which the
            // compositor has to be told: an axis event goes to the pointer's
            // focus, and the unnamed one went to the keyboard's instead.
            let Some(canvas) = target_canvas(&event) else {
                return;
            };
            let Some(id) = canvas_surface_id(&canvas) else {
                return;
            };
            event.prevent_default();
            let (dx, dy) = wheel_pixels(&event);
            send(
                &transport,
                InputEvent::PointerScroll {
                    id: SurfaceId(id),
                    dx,
                    dy,
                },
            );
        });
        // Not passive, or `prevent_default` above is ignored and the page
        // scrolls anyway.
        let options = web_sys::AddEventListenerOptions::new();
        options.set_passive(false);
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            let _ = doc.add_event_listener_with_callback_and_add_event_listener_options(
                "wheel",
                listener.as_ref().unchecked_ref(),
                &options,
            );
        }
        listener.forget();
    }
}

/// A wheel event's delta in pixels, whichever unit the browser chose to report.
///
/// `deltaMode` is pixels in Chrome but lines in Firefox, and a page for some
/// mice; taking `deltaY` at face value scrolls three pixels or a whole screen.
fn wheel_pixels(event: &WheelEvent) -> (f64, f64) {
    // A line is about one line of text; a page, about a screen of them.
    let scale = match event.delta_mode() {
        WheelEvent::DOM_DELTA_LINE => 16.0,
        WheelEvent::DOM_DELTA_PAGE => 400.0,
        _ => 1.0,
    };
    (event.delta_x() * scale, event.delta_y() * scale)
}
