//! `KeyboardEvent.code` to Linux evdev keycode, and the modifier table that
//! goes with it.

/// Browser `MouseEvent.button` → Linux `BTN_*` code.
pub fn evdev_button(button: i16) -> Option<u32> {
    match button {
        0 => Some(0x110), // BTN_LEFT
        1 => Some(0x112), // BTN_MIDDLE
        2 => Some(0x111), // BTN_RIGHT
        _ => None,
    }
}

/// `KeyboardEvent.code` → Linux evdev keycode (US layout, common subset).
#[allow(clippy::match_same_arms, clippy::too_many_lines)] // one arm per key
pub fn evdev_key(code: &str) -> Option<u32> {
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
        "NumpadMultiply" => 55,
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
        "NumLock" => 69,
        "ScrollLock" => 70,
        // The numeric keypad. Distinct scancodes from the digit row, and a
        // client reading them through xkb gets the arrows and Home/End that
        // NumLock-off produces — which is why mapping them to the digits above
        // would be wrong rather than merely approximate.
        "Numpad7" => 71,
        "Numpad8" => 72,
        "Numpad9" => 73,
        "NumpadSubtract" => 74,
        "Numpad4" => 75,
        "Numpad5" => 76,
        "Numpad6" => 77,
        "NumpadAdd" => 78,
        "Numpad1" => 79,
        "Numpad2" => 80,
        "Numpad3" => 81,
        "Numpad0" => 82,
        "NumpadDecimal" => 83,
        "IntlBackslash" => 86,
        "F11" => 87,
        "F12" => 88,
        "IntlRo" => 89,
        "NumpadEnter" => 96,
        "ControlRight" => 97,
        "NumpadDivide" => 98,
        "PrintScreen" => 99,
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
        "NumpadEqual" => 117,
        "Pause" => 119,
        "IntlYen" => 124,
        // The super key. Every desktop shortcut a client defines starts with
        // it, and without a scancode the browser's key event was dropped before
        // it reached the compositor — while `MODIFIERS` below already claimed
        // to hold it down, so the two could not agree about it either.
        "MetaLeft" => 125,
        "MetaRight" => 126,
        "ContextMenu" => 127,
        _ => return None,
    };
    Some(key)
}

/// Modifier name as the browser reports it, and the evdev keycodes that produce
/// it. Left and right count as the same modifier, because they are — except for
/// alt, where they are not.
///
/// On any layout with a third level — azerty, qwertz, the international qwertys
/// — the right-hand alt key is `AltGr`, which is `ISO_Level3_Shift` and not alt
/// at all. The browser says so: it reports `AltGraph`, and leaves `Alt` false.
/// Counted as alt, the reconciliation below saw a modifier the browser denied
/// holding and dutifully released it — before every single keystroke it was
/// meant to shift. The user pressed `AltGr` and `à` for `@` and got `à`.
pub const MODIFIERS: [(&str, &[u32]); 5] = [
    ("Shift", &[42, 54]),
    ("Control", &[29, 97]),
    ("Alt", &[56]),
    ("AltGraph", &[100]),
    ("Meta", &[125, 126]),
];
