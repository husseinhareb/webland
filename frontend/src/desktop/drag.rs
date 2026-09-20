//! Moving a window that the shell has no titlebar to grab: a client-driven move
//! — a GTK titlebar drag — or an alt-drag anywhere in the surface.
//!
//! It runs on the desktop element rather than in the window, because the pointer
//! is over the client's own surface: no piece of shell chrome saw the gesture
//! start, and the desktop is the one element every later pointer event reaches.
//!
//! What it does *not* do is move the window itself. A second implementation of
//! dragging is how these windows ended up being the only ones that could not be
//! snapped to an edge or dropped on a workspace — so once the grab is known,
//! this hands the gesture to the titlebar drag and every window moves the same
//! way.

use std::cell::Cell;
use std::rc::Rc;

use leptos::prelude::*;
use web_sys::PointerEvent;

use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

/// Where the pointer was last seen, held across the events of one gesture:
/// letting go has to say where it let go, and `pointerup` on the desktop need
/// not be over the window. `Rc<Cell<_>>` because the handlers are `Copy`
/// closures the view binds twice, and `StoredValue` because none of it is
/// `Send`.
type Last = StoredValue<Rc<Cell<Option<(f64, f64)>>>, LocalStorage>;

/// A client-driven move in progress, or waiting to be.
#[derive(Clone, Copy)]
pub struct ClientDrag {
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
    last: Last,
}

impl ClientDrag {
    pub fn new(
        scene: StoredValue<Scene, LocalStorage>,
        transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
    ) -> Self {
        Self {
            scene,
            transport,
            last: StoredValue::new_local(Rc::new(Cell::new(None))),
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
        let pointer = (event.client_x(), event.client_y());
        let (x, y) = if captured.get_untracked() {
            virtual_cursor.get_untracked().unwrap_or(pointer)
        } else {
            pointer
        };
        self.last.with_value(|last| last.set(Some((x, y))));
        let Some(transport) = self.transport.get_value() else {
            return;
        };
        self.scene.with_value(|scene| {
            // The client asks to be moved without saying from where, so the
            // grab is whatever the pointer happens to sit over on the first
            // event of the gesture. From then on it is an ordinary drag.
            if scene.titlebar_drag.get_untracked().is_none() {
                scene.start_titlebar_drag(id, x, y, &transport);
            }
            scene.update_titlebar_drag(x, y, &transport);
        });
    }

    /// Let go: snap, drop on a workspace, or just stay put.
    pub fn dropped(self) {
        let dragging = self.scene.with_value(|scene| scene.dragging);
        if dragging.get_untracked().is_none() {
            return;
        }
        dragging.set(None);
        let mut last = None;
        self.last.with_value(|held| last = held.replace(None));
        if let Some((x, y)) = last
            && let Some(transport) = self.transport.get_value()
        {
            self.scene
                .with_value(|scene| scene.finish_titlebar_drag(x, y, &transport));
        }
    }
}
