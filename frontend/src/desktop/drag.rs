//! Moving a window whose client asked to be moved — a GTK titlebar drag.
//!
//! It runs on the desktop element rather than in the window, because the pointer
//! is over the client's own surface: no piece of shell chrome saw the gesture
//! start, and the desktop is the one element every later pointer event reaches.

use std::cell::Cell;
use std::rc::Rc;

use leptos::prelude::*;
use web_sys::PointerEvent;

use crate::scene::{Scene, drag_origin};

/// Where the pointer sits relative to the window's corner, held across the
/// events of one gesture. `Rc<Cell<_>>` because the handlers are `Copy` closures
/// the view binds twice, and `StoredValue` because none of it is `Send`.
type Grab = StoredValue<Rc<Cell<Option<(f64, f64)>>>, LocalStorage>;

/// A client-driven move in progress, or waiting to be.
#[derive(Clone, Copy)]
pub struct ClientDrag {
    scene: StoredValue<Scene, LocalStorage>,
    /// Empty until the first pointer event of the gesture: the client asks to
    /// be moved without saying from where.
    grab: Grab,
}

impl ClientDrag {
    pub fn new(scene: StoredValue<Scene, LocalStorage>) -> Self {
        Self {
            scene,
            grab: StoredValue::new_local(Rc::new(Cell::new(None))),
        }
    }

    /// Follow the pointer, if a client asked to be followed.
    pub fn moved(self, event: &PointerEvent) {
        let (dragging, captured, virtual_cursor) = self
            .scene
            .with_value(|s| (s.dragging, s.captured, s.virtual_cursor));
        let Some(id) = dragging.get_untracked() else {
            return;
        };
        let at = if captured.get_untracked() {
            virtual_cursor
                .get_untracked()
                .unwrap_or((event.client_x(), event.client_y()))
        } else {
            (event.client_x(), event.client_y())
        };
        let grab = self.grab.with_value(|grab| grab.get()).unwrap_or_else(|| {
            let (x, y) = self.scene.with_value(|scene| scene.window_origin(id));
            (at.0 - f64::from(x), at.1 - f64::from(y))
        });
        self.grab.with_value(|held| held.set(Some(grab)));
        let (x, y) = drag_origin(at, grab);
        self.scene.with_value(|scene| scene.move_to(id, x, y));
    }

    /// Let go.
    pub fn dropped(self) {
        let dragging = self.scene.with_value(|scene| scene.dragging);
        if dragging.get_untracked().is_some() {
            dragging.set(None);
            self.grab.with_value(|grab| grab.set(None));
        }
    }
}
