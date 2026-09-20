//! Clipboard, both directions. The browser hands over a paste without a
//! permission prompt as long as it comes from its own `paste` event, which is
//! why the paste chord's key event is deferred until the text has been sent.
//!
//! That event is not always enough. It only fires where the browser thinks
//! something can be pasted *into*, so copying in an application outside the
//! browser and pressing the chord over a client's surface could reach the
//! compositor with nothing attached; the copy simply did not cross. So the
//! clipboard is also read whenever the page regains focus, which is exactly
//! when the user has come back from copying something somewhere else. That read
//! needs permission, asked for once by the browser; without it the `paste`
//! event remains the path and nothing is worse than before.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::KeyboardEvent;
use webland_protocol::{ClientMessage, InputEvent, Press, encode};

use crate::protocol::WebSocketTransport;

use super::wire::send;

/// A key event deferred by the `keydown` handler when a paste chord is
/// detected, so that `wire_paste` can send the clipboard text *first* and the
/// compositor has the selection set before the client sees the keystroke.
pub struct PendingPaste {
    pub keycode: u32,
}

/// Hold a paste chord's key event back until the clipboard text has been sent,
/// so the compositor has the selection set before the client asks for it.
///
/// The timeout is the safety net: a browser that fires no `paste` event at all (
/// an empty clipboard on some of them) would otherwise swallow the keystroke.
pub fn defer(
    transport: &Rc<WebSocketTransport>,
    pending: &Rc<RefCell<Option<PendingPaste>>>,
    keycode: u32,
) {
    *pending.borrow_mut() = Some(PendingPaste { keycode });
    let transport = transport.clone();
    let pending = pending.clone();
    let flush = Closure::once(move || flush_deferred(&transport, &pending));
    if let Some(window) = web_sys::window() {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            flush.as_ref().unchecked_ref(),
            50,
        );
    }
    flush.forget();
}

/// Send a deferred key, if one is still waiting.
fn flush_deferred(transport: &WebSocketTransport, pending: &RefCell<Option<PendingPaste>>) {
    if let Some(paste) = pending.borrow_mut().take() {
        send(
            transport,
            InputEvent::Key {
                keycode: paste.keycode,
                state: Press::Down,
            },
        );
    }
}

/// Is this the chord that pastes, either spelling of it?
pub fn is_paste(event: &KeyboardEvent) -> bool {
    let modified = event.ctrl_key() || event.meta_key();
    (modified && event.code() == "KeyV") || (event.shift_key() && event.code() == "Insert")
}

/// Send the browser's clipboard whenever the browser hands it over.
///
/// A `paste` event carries the text with it, so this needs no permission and no
/// prompt, unlike reading the clipboard directly, which needs both.
///
/// The `keydown` handler defers the paste chord's key event into
/// `pending_paste`. After the clipboard text is sent here, the deferred key is
/// flushed so the compositor processes it *after* the selection is set.
pub fn wire_paste(transport: Rc<WebSocketTransport>, pending: Rc<RefCell<Option<PendingPaste>>>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let listener = Closure::<dyn FnMut(web_sys::ClipboardEvent)>::new(
        move |event: web_sys::ClipboardEvent| {
            if let Some(data) = event.clipboard_data()
                && let Ok(text) = data.get_data("text/plain")
                && !text.is_empty()
            {
                // 1. Send the clipboard text to the compositor.
                if let Ok(bytes) = encode(&ClientMessage::Clipboard { text }) {
                    transport.send(&bytes);
                }
            }
            // 2. Now flush the deferred key event so the compositor injects
            //    the keystroke *after* the selection is already set.
            flush_deferred(&transport, &pending);
        },
    );
    let _ = window.add_event_listener_with_callback("paste", listener.as_ref().unchecked_ref());
    listener.forget();
}

/// Send the browser's clipboard whenever the page is returned to.
///
/// The last text sent is remembered, so coming back to a tab a dozen times does
/// not send the same paragraph a dozen times, and so the text this browser was
/// *given* by a client is not immediately handed back to the compositor as if
/// the user had copied it outside.
pub fn sync_on_focus(transport: Rc<WebSocketTransport>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let last: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let listener = Closure::<dyn FnMut()>::new(move || {
        let transport = transport.clone();
        let last = last.clone();
        let Some(clipboard) = web_sys::window().map(|window| window.navigator().clipboard()) else {
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            // Denied permission, an empty clipboard, or an image: all of them
            // land here, and all of them mean there is nothing to send.
            let Ok(text) = wasm_bindgen_futures::JsFuture::from(clipboard.read_text()).await else {
                return;
            };
            let Some(text) = text.as_string() else {
                return;
            };
            if text.is_empty() || *last.borrow() == text {
                return;
            }
            last.replace(text.clone());
            if let Ok(bytes) = encode(&ClientMessage::Clipboard { text }) {
                transport.send(&bytes);
            }
        });
    });
    let _ = window.add_event_listener_with_callback("focus", listener.as_ref().unchecked_ref());
    listener.forget();
}

/// Put text a client copied onto the browser's own clipboard.
///
/// Fire and forget: the write is allowed while the page still holds the user
/// activation from the copy that caused it, and there is nothing useful to do
/// when it is not; the text is gone by then anyway.
pub fn set_clipboard(text: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.navigator().clipboard().write_text(text);
    }
}
