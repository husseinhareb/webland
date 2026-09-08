//! Browser input capture (Phase 3).
//!
//! Pointer events on the canvas and key events on the window are translated to
//! `webland-protocol` [`InputEvent`]s and sent to the backend, which injects
//! them into the Wayland seat. Keyboard mapping is `KeyboardEvent.code` →
//! Linux evdev keycode; it covers a common subset, not (yet) IME or every key.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Element, HtmlCanvasElement, KeyboardEvent, PointerEvent, WheelEvent};
use webland_core::Point;
use webland_protocol::{ClientMessage, InputEvent, Press, encode};

use crate::latency::Latency;
use crate::protocol::{Transport, WebSocketTransport};

/// Attach pointer (canvas) and keyboard (window) listeners that stream input.
pub fn wire(container: &Element, transport: Rc<WebSocketTransport>, latency: Rc<Latency>) {
    // Bound on the desktop and routed by event target, so windows that open
    // later need no wiring of their own.
    // Pointer motion.
    {
        let transport = transport.clone();
        let listener = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            let Some(canvas) = target_canvas(&event) else {
                return;
            };
            if let Some(position) = surface_position(&canvas, &event) {
                send(&transport, InputEvent::PointerMotion { position });
            }
        });
        let _ = container
            .add_event_listener_with_callback("pointermove", listener.as_ref().unchecked_ref());
        listener.forget();
    }

    // Pointer buttons.
    for (name, press) in [("pointerdown", Press::Down), ("pointerup", Press::Up)] {
        let transport = transport.clone();
        let latency = latency.clone();
        let listener = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            // Only clicks that land on a surface are input; the chrome around it
            // belongs to the shell, and raising and focusing happen there.
            if target_canvas(&event).is_none() {
                return;
            }
            if let Some(button) = evdev_button(event.button()) {
                if press == Press::Down {
                    latency.input_sent();
                }
                send(
                    &transport,
                    InputEvent::PointerButton {
                        button,
                        state: press,
                    },
                );
            }
        });
        let _ = container.add_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
        listener.forget();
    }

    // Wheel. Without this the page scrolls under the desktop instead of the
    // application scrolling inside its window, which is the wrong thing in the
    // most confusing possible way: it looks like the click went somewhere.
    {
        let transport = transport.clone();
        let listener = Closure::<dyn FnMut(WheelEvent)>::new(move |event: WheelEvent| {
            if target_canvas(&event).is_none() {
                return;
            }
            event.prevent_default();
            let (dx, dy) = wheel_pixels(&event);
            send(&transport, InputEvent::PointerScroll { dx, dy });
        });
        // Not passive, or `prevent_default` above is ignored and the page
        // scrolls anyway.
        let options = web_sys::AddEventListenerOptions::new();
        options.set_passive(false);
        let _ = container.add_event_listener_with_callback_and_add_event_listener_options(
            "wheel",
            listener.as_ref().unchecked_ref(),
            &options,
        );
        listener.forget();
    }

    // Keyboard, on the window so keys are captured without focusing the canvas.
    if let Some(window) = web_sys::window() {
        // What the compositor currently believes is held down. The compositor
        // derives modifier state from key events alone, exactly as a real
        // keyboard would, so this is the only place the two can drift apart.
        let held: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
        for (name, press) in [("keydown", Press::Down), ("keyup", Press::Up)] {
            let transport = transport.clone();
            let held = held.clone();
            let latency = latency.clone();
            let listener = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
                // Keys aimed at the shell stay in the shell. The listener is on
                // the window so that applications get keys without the canvas
                // needing focus, which also means the launcher's search box
                // would otherwise type into whichever client has the seat — and
                // be counted as interaction latency while doing it.
                if aimed_at_shell(&event) {
                    return;
                }
                let Some(keycode) = evdev_key(&event.code()) else {
                    return;
                };
                // This key belongs to the application now, so the browser must
                // not also act on it — arrows and space scroll the page, tab
                // walks the shell's own buttons, and `/` opens a find bar.
                event.prevent_default();
                if press == Press::Down {
                    latency.input_sent();
                }
                reconcile_modifiers(&transport, &held, &event, keycode);
                send(
                    &transport,
                    InputEvent::Key {
                        keycode,
                        state: press,
                    },
                );
                // Track the modifier keys themselves, so the reconciliation
                // above does not re-send what this event already carried.
                if MODIFIERS.iter().any(|(_, codes)| codes.contains(&keycode)) {
                    let mut held = held.borrow_mut();
                    held.retain(|&code| code != keycode);
                    if press == Press::Down {
                        held.push(keycode);
                    }
                }
            });
            let _ =
                window.add_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
            listener.forget();
        }
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

/// Modifier name as the browser reports it, and the evdev keycodes that produce
/// it. Left and right count as the same modifier, because they are.
const MODIFIERS: [(&str, &[u32]); 4] = [
    ("Shift", &[42, 54]),
    ("Control", &[29, 97]),
    ("Alt", &[56, 100]),
    ("Meta", &[125, 126]),
];

/// Send whatever key events the compositor needs to agree with the browser about
/// which modifiers are held.
///
/// Without this a chord only works if we saw the modifier's own keydown, which
/// is not something to rely on: the browser eats some of them, focus can arrive
/// mid-chord with a modifier already down, and a synthesised event may carry
/// `shiftKey` with no `ShiftLeft` event at all. The symptom is a modifier that
/// silently does nothing, or worse, one that stays stuck down afterwards.
fn reconcile_modifiers(
    transport: &WebSocketTransport,
    held: &Rc<RefCell<Vec<u32>>>,
    event: &KeyboardEvent,
    keycode: u32,
) {
    for (name, codes) in MODIFIERS {
        // The event being dispatched is this modifier: it speaks for itself.
        if codes.contains(&keycode) {
            continue;
        }
        let wanted = event.get_modifier_state(name);
        let current = held.borrow().iter().any(|code| codes.contains(code));
        if wanted == current {
            continue;
        }
        // Left-hand keycode by convention; the client cannot tell which it was.
        let code = codes[0];
        let state = if wanted { Press::Down } else { Press::Up };
        send(
            transport,
            InputEvent::Key {
                keycode: code,
                state,
            },
        );
        let mut held = held.borrow_mut();
        held.retain(|&existing| !codes.contains(&existing));
        if wanted {
            held.push(code);
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
        // `offset_x`/`offset_y` are already f64 under web-sys's unstable cfg,
        // which the frontend builds with for WebCodecs.
        x: event.offset_x() * f64::from(canvas.width()) / displayed_w,
        y: event.offset_y() * f64::from(canvas.height()) / displayed_h,
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

/// Is this key event meant for the shell rather than an application?
///
/// Anything typed into a form control belongs to the chrome around the surfaces,
/// not to the surfaces themselves.
fn aimed_at_shell(event: &KeyboardEvent) -> bool {
    let Some(target) = event.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
        return false;
    };
    matches!(
        target.tag_name().to_ascii_uppercase().as_str(),
        "INPUT" | "TEXTAREA" | "SELECT" | "BUTTON"
    )
}

/// The canvas an event landed on, if it landed on one at all.
fn target_canvas(event: &web_sys::Event) -> Option<HtmlCanvasElement> {
    event.target()?.dyn_into::<HtmlCanvasElement>().ok()
}
