//! Keyboard input, listening on the window so keys reach applications without
//! the canvas needing focus.

use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::KeyboardEvent;
use webland_protocol::{InputEvent, Press};

use crate::latency::Latency;
use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

use super::clipboard::{self, PendingPaste, is_paste, wire_paste};
use super::dom::aimed_at_shell;
use super::keymap::{MODIFIERS, evdev_key};
use super::shortcuts;
use super::wire::send;

#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
export function lock_keyboard() {
    try {
        if (navigator.keyboard && navigator.keyboard.lock) {
            navigator.keyboard.lock();
        }
    } catch (e) {}
}
export function unlock_keyboard() {
    try {
        if (navigator.keyboard && navigator.keyboard.unlock) {
            navigator.keyboard.unlock();
        }
    } catch (e) {}
}
"#)]
extern "C" {
    pub fn lock_keyboard();
    pub fn unlock_keyboard();
}

/// Attach the key and blur listeners, and the paste handler they defer into.
pub fn install(scene: &Scene, transport: &Rc<WebSocketTransport>, latency: &Rc<Latency>) {
    let pending_paste: Rc<RefCell<Option<PendingPaste>>> = Rc::new(RefCell::new(None));
    wire_paste(transport.clone(), pending_paste.clone());
    clipboard::sync_on_focus(transport.clone());

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
            let scene = scene.clone();
            let pending_paste = pending_paste.clone();
            let listener = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
                // Keys aimed at the shell stay in the shell. The listener is on
                // the window so that applications get keys without the canvas
                // needing focus, which also means the launcher's search box
                // would otherwise type into whichever client has the seat, and
                // be counted as interaction latency while doing it.
                if aimed_at_shell(&event) {
                    return;
                }

                // Keys must not reach client windows when the launcher is open.
                if scene.launcher_open.get_untracked() {
                    if event.key() == "Escape" {
                        event.prevent_default();
                        scene.launcher_open.set(false);
                        return;
                    }
                    if let Some(doc) = web_sys::window().and_then(|w| w.document())
                        && let Some(el) =
                            doc.query_selector("#webland-panel .search").ok().flatten()
                        && let Ok(input) = el.dyn_into::<web_sys::HtmlInputElement>()
                    {
                        let _ = input.focus();
                    }
                    event.prevent_default();
                    return;
                }

                // Escape closes the switcher; releasing alt commits it.
                if event.code() == "Escape" && scene.alt_tab.get_untracked().is_some() {
                    event.prevent_default();
                    scene.alt_tab.set(None);
                    return;
                }
                if press == Press::Up && matches!(event.code().as_str(), "AltLeft" | "AltRight") {
                    shortcuts::commit_alt_tab(&scene, &transport);
                }
                if shortcuts::claims(&event) {
                    event.prevent_default();
                    shortcuts::handle(&scene, &transport, &event, press);
                    return;
                }
                let Some(keycode) = evdev_key(&event.code()) else {
                    return;
                };
                // This key belongs to the application now, so the browser must
                // not also act on it; arrows and space scroll the page, tab
                // walks the shell's own buttons, and `/` opens a find bar.
                //
                // Paste is the exception: the browser must be allowed to run
                // its default handling so the `paste` event fires, which is
                // the only way a page is handed the clipboard text without a
                // permission prompt.
                //
                // On `Press::Down` of a paste chord the key event is
                // *deferred*: the `paste` handler sends the clipboard text
                // first, then flushes the stored key, so the compositor has
                // the selection set before the client sees the keystroke and
                // asks for it.
                if is_paste(&event) && press == Press::Down {
                    // No `prevent_default`: the browser's own paste event is
                    // the payload, and preventing it never fires.
                    latency.input_sent();
                    reconcile_modifiers(&transport, &held, &event, keycode);
                    clipboard::defer(&transport, &pending_paste, keycode);
                    hold(&held, keycode, Press::Down);
                    return;
                }
                if !is_paste(&event) {
                    event.prevent_default();
                }
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
                hold(&held, keycode, press);
            });
            let _ =
                window.add_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
            listener.forget();
        }

        // A page that loses focus stops being told about keys, so whatever was
        // held when it went away is never released: alt-tab out of a chord and
        // the compositor holds those modifiers for good. From then on every
        // keystroke reaches the client as a chord, letters do nothing and
        // return splits the terminal, and no later key event disagrees with
        // `held`, so the reconciliation above never notices. Let go on the way
        // out, which is the one moment the browser still tells us about.
        {
            let transport = transport.clone();
            let held = held.clone();
            let scene = scene.clone();
            let listener = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                scene.alt_tab.set(None);
                let letting_go = std::mem::take(&mut *held.borrow_mut());
                for keycode in letting_go {
                    send(
                        &transport,
                        InputEvent::Key {
                            keycode,
                            state: Press::Up,
                        },
                    );
                }
            });
            let _ =
                window.add_event_listener_with_callback("blur", listener.as_ref().unchecked_ref());
            listener.forget();
        }
    }
}

/// Record a modifier as held or released, ignoring every other key.
fn hold(held: &Rc<RefCell<Vec<u32>>>, keycode: u32, press: Press) {
    if !MODIFIERS.iter().any(|(_, codes)| codes.contains(&keycode)) {
        return;
    }
    let mut held = held.borrow_mut();
    held.retain(|&code| code != keycode);
    if press == Press::Down {
        held.push(keycode);
    }
}

/// Tell the compositor that nothing is held down.
///
/// The seat outlives any one page: a modifier still down when a tab is closed or
/// reloaded stays down, and the new page has no way to find out; it knows only
/// what it sent itself, which is nothing yet. So a page that has just connected
/// says what is true of a keyboard that has just been plugged in. Releasing a
/// key that was not held costs nothing; the compositor ignores it.
pub fn release_modifiers(transport: &WebSocketTransport) {
    for (_, codes) in MODIFIERS {
        for &keycode in codes {
            send(
                transport,
                InputEvent::Key {
                    keycode,
                    state: Press::Up,
                },
            );
        }
    }
}

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
    // Windows has no level-3 key of its own and spells `AltGr` as ctrl+alt, so a
    // browser there reports all three at once. Taking the other two at face
    // value would hand the client a control chord for every accented character;
    // the `AltGraph` above is the one that is true everywhere.
    let altgr = event.get_modifier_state("AltGraph");
    for (name, codes) in MODIFIERS {
        // The event being dispatched is this modifier: it speaks for itself.
        if codes.contains(&keycode) {
            continue;
        }
        if altgr && matches!(name, "Alt" | "Control") {
            continue;
        }
        let wanted = event.get_modifier_state(name);
        let current = held.borrow().iter().any(|code| codes.contains(code));
        if wanted == current {
            continue;
        }
        if wanted {
            // Left-hand keycode by convention; the client cannot tell which.
            send(
                transport,
                InputEvent::Key {
                    keycode: codes[0],
                    state: Press::Down,
                },
            );
            held.borrow_mut().push(codes[0]);
            continue;
        }
        // Release whichever key is actually down, not the left-hand one by
        // convention: letting go of `ControlLeft` while `ControlRight` is the
        // one held leaves the modifier stuck in the compositor for good, and
        // clears `held`, so nothing here ever notices or tries again.
        let down = held
            .borrow()
            .iter()
            .copied()
            .filter(|code| codes.contains(code))
            .collect::<Vec<_>>();
        for keycode in down {
            send(
                transport,
                InputEvent::Key {
                    keycode,
                    state: Press::Up,
                },
            );
        }
        held.borrow_mut().retain(|code| !codes.contains(code));
    }
}
