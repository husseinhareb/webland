//! Browser input capture (Phase 3).
//!
//! Pointer events on the canvas and key events on the window are translated to
//! `webland-protocol` [`InputEvent`]s and sent to the backend, which injects
//! them into the Wayland seat. Keyboard mapping is `KeyboardEvent.code` →
//! Linux evdev keycode; it covers a common subset, not (yet) IME or every key.

use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{HtmlCanvasElement, KeyboardEvent, PointerEvent};
use webland_core::Point;
use webland_protocol::{ClientMessage, InputEvent, Press, encode};

use crate::protocol::{Transport, WebSocketTransport};

/// Attach pointer (canvas) and keyboard (window) listeners that stream input.
pub fn wire(canvas: &HtmlCanvasElement, transport: Rc<WebSocketTransport>) {
    // Pointer motion.
    {
        let transport = transport.clone();
        let canvas_ref = canvas.clone();
        let listener = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            if let Some(position) = surface_position(&canvas_ref, &event) {
                send(&transport, InputEvent::PointerMotion { position });
            }
        });
        let _ = canvas
            .add_event_listener_with_callback("pointermove", listener.as_ref().unchecked_ref());
        listener.forget();
    }

    // Pointer buttons.
    for (name, press) in [("pointerdown", Press::Down), ("pointerup", Press::Up)] {
        let transport = transport.clone();
        let listener = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            if let Some(button) = evdev_button(event.button()) {
                send(
                    &transport,
                    InputEvent::PointerButton {
                        button,
                        state: press,
                    },
                );
            }
        });
        let _ = canvas.add_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
        listener.forget();
    }

    // Keyboard, on the window so keys are captured without focusing the canvas.
    if let Some(window) = web_sys::window() {
        for (name, press) in [("keydown", Press::Down), ("keyup", Press::Up)] {
            let transport = transport.clone();
            let listener = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
                if let Some(keycode) = evdev_key(&event.code()) {
                    send(
                        &transport,
                        InputEvent::Key {
                            keycode,
                            state: press,
                        },
                    );
                }
            });
            let _ =
                window.add_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
            listener.forget();
        }
    }
}

fn send(transport: &WebSocketTransport, event: InputEvent) {
    if let Ok(bytes) = encode(&ClientMessage::Input(event)) {
        transport.send(&bytes);
    }
}

/// Map a pointer event's canvas-local offset to surface pixels (the canvas may
/// be CSS-scaled, so scale by the ratio of backing size to displayed size).
fn surface_position(canvas: &HtmlCanvasElement, event: &PointerEvent) -> Option<Point> {
    let displayed_w = f64::from(canvas.client_width());
    let displayed_h = f64::from(canvas.client_height());
    if displayed_w <= 0.0 || displayed_h <= 0.0 {
        return None;
    }
    Some(Point {
        x: f64::from(event.offset_x()) * f64::from(canvas.width()) / displayed_w,
        y: f64::from(event.offset_y()) * f64::from(canvas.height()) / displayed_h,
    })
}

/// Browser `MouseEvent.button` → Linux `BTN_*` code.
fn evdev_button(button: i16) -> Option<u32> {
    match button {
        0 => Some(0x110), // BTN_LEFT
        1 => Some(0x112), // BTN_MIDDLE
        2 => Some(0x111), // BTN_RIGHT
        _ => None,
    }
}

/// `KeyboardEvent.code` → Linux evdev keycode (US layout, common subset).
#[allow(clippy::match_same_arms)]
fn evdev_key(code: &str) -> Option<u32> {
    let key = match code {
        "Escape" => 1,
        "Digit1" => 2,
        "Digit2" => 3,
        "Digit3" => 4,
        "Digit4" => 5,
        "Digit5" => 6,
        "Digit6" => 7,
        "Digit7" => 8,
        "Digit8" => 9,
        "Digit9" => 10,
        "Digit0" => 11,
        "Minus" => 12,
        "Equal" => 13,
        "Backspace" => 14,
        "Tab" => 15,
        "KeyQ" => 16,
        "KeyW" => 17,
        "KeyE" => 18,
        "KeyR" => 19,
        "KeyT" => 20,
        "KeyY" => 21,
        "KeyU" => 22,
        "KeyI" => 23,
        "KeyO" => 24,
        "KeyP" => 25,
        "BracketLeft" => 26,
        "BracketRight" => 27,
        "Enter" => 28,
        "ControlLeft" => 29,
        "KeyA" => 30,
        "KeyS" => 31,
        "KeyD" => 32,
        "KeyF" => 33,
        "KeyG" => 34,
        "KeyH" => 35,
        "KeyJ" => 36,
        "KeyK" => 37,
        "KeyL" => 38,
        "Semicolon" => 39,
        "Quote" => 40,
        "Backquote" => 41,
        "ShiftLeft" => 42,
        "Backslash" => 43,
        "KeyZ" => 44,
        "KeyX" => 45,
        "KeyC" => 46,
        "KeyV" => 47,
        "KeyB" => 48,
        "KeyN" => 49,
        "KeyM" => 50,
        "Comma" => 51,
        "Period" => 52,
        "Slash" => 53,
        "ShiftRight" => 54,
        "AltLeft" => 56,
        "Space" => 57,
        "CapsLock" => 58,
        "F1" => 59,
        "F2" => 60,
        "F3" => 61,
        "F4" => 62,
        "F5" => 63,
        "F6" => 64,
        "F7" => 65,
        "F8" => 66,
        "F9" => 67,
        "F10" => 68,
        "F11" => 87,
        "F12" => 88,
        "ControlRight" => 97,
        "AltRight" => 100,
        "Home" => 102,
        "ArrowUp" => 103,
        "PageUp" => 104,
        "ArrowLeft" => 105,
        "ArrowRight" => 106,
        "End" => 107,
        "ArrowDown" => 108,
        "PageDown" => 109,
        "Insert" => 110,
        "Delete" => 111,
        _ => return None,
    };
    Some(key)
}
