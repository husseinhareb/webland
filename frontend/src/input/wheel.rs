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
                {
                    if let Some(canvas) = el.dyn_ref::<HtmlCanvasElement>()
                        && let Some(id) = canvas_surface_id(canvas)
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
                    } else {
                        event.prevent_default();
                        let (dx, dy) = wheel_pixels(&event);
                        scroll_dom(&el, dx, dy);
                    }
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

/// Scroll the nearest scrollable DOM ancestor under the pointer.
fn scroll_dom(el: &web_sys::Element, dx: f64, dy: f64) {
    let mut curr = Some(el.clone());
    while let Some(node) = curr {
        if node.id() == "webland-desktop" || node.tag_name().eq_ignore_ascii_case("BODY") {
            break;
        }
        let can_scroll_y = node.scroll_height() > node.client_height();
        let can_scroll_x = node.scroll_width() > node.client_width();
        if (can_scroll_y && dy != 0.0) || (can_scroll_x && dx != 0.0) {
            if let Some(window) = web_sys::window()
                && let Ok(Some(style)) = window.get_computed_style(&node)
            {
                let overflow_y = style.get_property_value("overflow-y").unwrap_or_default();
                let overflow_x = style.get_property_value("overflow-x").unwrap_or_default();
                let allows_y = !matches!(overflow_y.as_str(), "hidden" | "visible" | "");
                let allows_x = !matches!(overflow_x.as_str(), "hidden" | "visible" | "");
                if (allows_y && dy != 0.0) || (allows_x && dx != 0.0) {
                    node.scroll_by_with_x_and_y(dx, dy);
                    return;
                }
            } else {
                node.scroll_by_with_x_and_y(dx, dy);
                return;
            }
        }
        curr = node.parent_element();
    }
}
