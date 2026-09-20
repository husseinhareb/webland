//! Pointer input. Bound on the document and routed by event target, so a window
//! that opens later needs no wiring of its own.
//!
//! Only the unlocked case is here: the browser routes the event, and the shell
//! forwards what landed on a canvas and leaves its own chrome to Leptos. With the
//! pointer locked there is no routing left to use. See [`super::locked`].

use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use web_sys::{Document, Element, MouseEvent};
use webland_core::SurfaceId;
use webland_protocol::{InputEvent, Press};

use crate::latency::Latency;
use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

use super::dom::{canvas_surface_id, surface_position, target_canvas};
use super::keymap::evdev_button;
use super::locked;
use super::wire::send;

/// Where the last click landed, so the next one can tell a double-click from two.
pub struct ClickState {
    pub time: f64,
    pub x: f64,
    pub y: f64,
    pub window_id: Option<u64>,
    pub is_titlebar: bool,
}

pub type LastClick = Rc<RefCell<Option<ClickState>>>;

/// What the last press landed on, so the release can tell a click from a drag
/// that happened to end on a button.
pub type Pressed = Rc<RefCell<Option<Element>>>;

/// Attach the motion, context-menu and button listeners.
pub fn install(
    container: &Element,
    scene: &Scene,
    transport: &Rc<WebSocketTransport>,
    latency: &Rc<Latency>,
) {
    let last_click: LastClick = Rc::new(RefCell::new(None));
    let pressed: Pressed = Rc::new(RefCell::new(None));

    {
        let transport = transport.clone();
        let scene = scene.clone();
        on_document("mousemove", move |event: &MouseEvent| {
            let doc = document();
            if let Some(doc) = locked_document(doc.as_ref()) {
                locked::motion(&scene, &transport, Some(doc), event);
                return;
            }
            scene
                .virtual_cursor
                .set(Some((event.client_x(), event.client_y())));
            // Named, because the window under the cursor is not always the
            // focused one: an unnamed motion went to whatever held the focus, so
            // hovering one window pointed inside another.
            if let Some(canvas) = target_canvas(event)
                && let Some(id) = canvas_surface_id(&canvas)
                && let Some(position) = surface_position(&canvas, event)
            {
                send(
                    &transport,
                    InputEvent::PointerMotion {
                        id: SurfaceId(id),
                        position,
                    },
                );
            }
        });
    }

    // Right-clicking inside Webland belongs to the guest applications, not to
    // the browser's context menu.
    {
        let listener = wasm_bindgen::closure::Closure::<dyn FnMut(MouseEvent)>::new(
            move |event: MouseEvent| event.prevent_default(),
        );
        let _ = container.add_event_listener_with_callback(
            "contextmenu",
            wasm_bindgen::JsCast::unchecked_ref(listener.as_ref()),
        );
        listener.forget();
    }

    for (name, press) in [("mousedown", Press::Down), ("mouseup", Press::Up)] {
        let transport = transport.clone();
        let latency = latency.clone();
        let scene = scene.clone();
        let last_click = last_click.clone();
        let pressed = pressed.clone();
        on_document(name, move |event: &MouseEvent| {
            let doc = document();
            if let Some(doc) = locked_document(doc.as_ref()) {
                // A client holding the pointer for its own camera gets the
                // button raw; nothing of the shell is reachable while it does.
                if locked::grabbed_by_client(&scene) {
                    forward_button(&transport, &latency, event, press);
                    return;
                }
                let at = scene.virtual_cursor.get_untracked().unwrap_or((0.0, 0.0));
                if press == Press::Up {
                    locked::release(&scene, &transport, &pressed, Some(doc), event, at);
                } else {
                    locked::press(
                        &scene,
                        &transport,
                        &latency,
                        (&last_click, &pressed),
                        Some(doc),
                        event,
                        at,
                    );
                }
                return;
            }
            if target_canvas(event).is_some() {
                forward_button(&transport, &latency, event, press);
            }
        });
    }
}

fn forward_button(
    transport: &WebSocketTransport,
    latency: &Latency,
    event: &MouseEvent,
    press: Press,
) {
    if let Some(button) = evdev_button(event.button()) {
        if press == Press::Down {
            latency.input_sent();
        }
        send(
            transport,
            InputEvent::PointerButton {
                button,
                state: press,
            },
        );
    }
}

fn document() -> Option<Document> {
    web_sys::window().and_then(|w| w.document())
}

/// The document, but only while it holds the pointer lock.
fn locked_document(doc: Option<&Document>) -> Option<&Document> {
    doc.filter(|d| d.pointer_lock_element().is_some())
}

/// Attach a mouse listener to the document, for the lifetime of the page.
fn on_document(name: &str, mut handler: impl FnMut(&MouseEvent) + 'static) {
    let listener =
        wasm_bindgen::closure::Closure::<dyn FnMut(MouseEvent)>::new(move |event: MouseEvent| {
            handler(&event);
        });
    if let Some(doc) = document() {
        let _ = doc.add_event_listener_with_callback(
            name,
            wasm_bindgen::JsCast::unchecked_ref(listener.as_ref()),
        );
    }
    listener.forget();
}
