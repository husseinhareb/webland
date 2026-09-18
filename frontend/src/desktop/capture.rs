//! Pointer lock and fullscreen.
//!
//! The browser owns both, and only tells the page after the fact — a lock can be
//! refused, and either can be dropped with Escape without asking. So the scene's
//! `captured` and `fullscreen` signals are set from the browser's own events, not
//! from the call that asked, and everything here goes one way.

use leptos::prelude::*;

use crate::input::{lock_keyboard, unlock_keyboard, viewport};
use crate::scene::Scene;

/// Follow the browser's pointer-lock and fullscreen state for the lifetime of
/// the page.
pub fn watch(scene: StoredValue<Scene, LocalStorage>) {
    let captured = scene.with_value(|scene| scene.captured);
    let fullscreen = scene.with_value(|scene| scene.fullscreen);
    let virtual_cursor = scene.with_value(|scene| scene.virtual_cursor);

    on_document("pointerlockchange", move || {
        let locked = pointer_locked();
        captured.set(locked);
        mark_body(locked);
        if locked {
            lock_keyboard();
            // The virtual cursor starts in the middle: the real one is gone, and
            // wherever it was is no longer where the user is looking.
            let (w, h) = viewport();
            virtual_cursor.set(Some((w / 2.0, h / 2.0)));
        } else {
            unlock_keyboard();
        }
    });
    on_document("pointerlockerror", move || {
        captured.set(false);
        mark_body(false);
        unlock_keyboard();
        scene.with_value(|s| {
            s.show_toast(
                "Cursor Lock Denied",
                Some(String::from("Pointer lock was rejected by the browser.")),
            );
        });
    });
    on_document("fullscreenchange", move || {
        let is_fullscreen = document().and_then(|d| d.fullscreen_element()).is_some();
        fullscreen.set(is_fullscreen);
    });
}

/// Give the pointer back. Escape does the same thing; this is the button.
pub fn release(scene: StoredValue<Scene, LocalStorage>) {
    if let Some(doc) = document() {
        doc.exit_pointer_lock();
    }
    mark_body(false);
    unlock_keyboard();
    scene.with_value(|scene| scene.captured.set(false));
}

/// Take the pointer, or give it back if it is already taken.
pub fn toggle(scene: StoredValue<Scene, LocalStorage>) {
    let Some(doc) = document() else {
        return;
    };
    if doc.pointer_lock_element().is_some() {
        release(scene);
        scene.with_value(|s| s.show_toast("Cursor Released", None));
        return;
    }
    let virtual_cursor = scene.with_value(|s| s.virtual_cursor);
    if virtual_cursor.get_untracked().is_none() {
        let (w, h) = viewport();
        virtual_cursor.set(Some((w / 2.0, h / 2.0)));
    }
    // The desktop element rather than the document, so the lock is scoped to
    // what Webland draws; the document is the fallback for a page that somehow
    // has no desktop yet.
    if let Some(desktop) = doc.query_selector("#webland-desktop").ok().flatten() {
        desktop.request_pointer_lock();
    } else if let Some(el) = doc.document_element() {
        el.request_pointer_lock();
    }
    scene.with_value(|s| {
        s.show_toast(
            "Cursor Locked",
            Some(String::from("Press ESC anytime to release")),
        );
    });
}

/// Go fullscreen, or come back.
pub fn toggle_fullscreen(scene: StoredValue<Scene, LocalStorage>) {
    let Some(doc) = document() else {
        return;
    };
    if doc.fullscreen_element().is_some() {
        doc.exit_fullscreen();
        scene.with_value(|s| s.show_toast("Exited Fullscreen", None));
    } else if let Some(el) = doc.document_element() {
        let _ = el.request_fullscreen();
        scene.with_value(|s| s.show_toast("Entered Fullscreen", None));
    }
}

fn document() -> Option<web_sys::Document> {
    web_sys::window().and_then(|w| w.document())
}

fn pointer_locked() -> bool {
    document().and_then(|d| d.pointer_lock_element()).is_some()
}

/// The stylesheet hides the real cursor off this class.
fn mark_body(locked: bool) {
    if let Some(body) = document().and_then(|d| d.body()) {
        let classes = body.class_list();
        let _ = if locked {
            classes.add_1("pointer-locked")
        } else {
            classes.remove_1("pointer-locked")
        };
    }
}

fn on_document(name: &str, handler: impl FnMut() + 'static) {
    let listener = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(handler);
    if let Some(doc) = document() {
        let _ = doc.add_event_listener_with_callback(
            name,
            wasm_bindgen::JsCast::unchecked_ref(listener.as_ref()),
        );
    }
    listener.forget();
}
