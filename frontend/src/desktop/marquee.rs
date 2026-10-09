//! Desktop marquee selection rectangle (rubber-band selection).
//!
//! Drawn when clicking and dragging on the empty wallpaper/desktop background,
//! tracking the pointer across all four quadrants with translucent blue styling.

use leptos::html::Main;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Element, KeyboardEvent, PointerEvent};

use crate::input::viewport;
use crate::scene::Scene;

/// An active marquee rectangle on the desktop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarqueeRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[allow(dead_code)]
impl MarqueeRect {
    /// Whether this marquee rectangle intersects another rectangle (e.g. desktop icons or windows).
    #[must_use]
    pub fn intersects(&self, other: &MarqueeRect) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }

    /// Whether a point `(px, py)` is contained within this marquee rectangle.
    #[must_use]
    pub fn contains_point(&self, px: f64, py: f64) -> bool {
        px >= self.x && px <= self.x + self.width && py >= self.y && py <= self.y + self.height
    }
}

/// Gesture tracker for desktop rubber-band selection.
#[derive(Clone, Copy, Debug)]
pub struct DesktopMarquee {
    rect: RwSignal<Option<MarqueeRect>>,
    anchor: StoredValue<Option<(f64, f64)>, LocalStorage>,
    pointer_id: StoredValue<Option<i32>, LocalStorage>,
    desktop_ref: NodeRef<Main>,
    scene: StoredValue<Scene, LocalStorage>,
}

impl DesktopMarquee {
    #[must_use]
    pub fn new(desktop_ref: NodeRef<Main>, scene: StoredValue<Scene, LocalStorage>) -> Self {
        let marquee = Self {
            rect: RwSignal::new(None),
            anchor: StoredValue::new_local(None),
            pointer_id: StoredValue::new_local(None),
            desktop_ref,
            scene,
        };
        marquee.watch_escape();
        marquee
    }

    /// Whether a marquee selection drag is currently in progress.
    #[must_use]
    pub fn is_active(self) -> bool {
        self.anchor.get_value().is_some()
    }

    /// The current marquee rectangle to display, if any.
    #[must_use]
    pub fn rect(self) -> Option<MarqueeRect> {
        self.rect.get()
    }

    /// Start a marquee selection gesture if the click landed on the desktop background.
    pub fn down(self, event: &PointerEvent) {
        if event.button() != 0 {
            return;
        }
        let captured = self.scene.with_value(|s| s.captured.get_untracked());
        if captured && event.is_trusted() {
            return;
        }
        let Some(target) = event.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
            return;
        };
        // Only start if the click landed directly on the desktop background
        if target.id() == "webland-desktop" {
            // Deselect any focused window and close the launcher when clicking empty desktop
            self.scene.with_value(|s| {
                s.focused.set(None);
                s.launcher_open.set(false);
            });

            if let Some(desktop) = self.desktop_ref.get() {
                let pid = event.pointer_id();
                let _ = desktop.set_pointer_capture(pid);
                self.pointer_id.set_value(Some(pid));
            }
            let (vw, vh) = viewport();
            let x = event.client_x().clamp(0.0, vw);
            let y = event.client_y().clamp(0.0, vh);
            self.anchor.set_value(Some((x, y)));
            self.rect.set(None);
        }
    }

    /// Update the marquee rectangle as the pointer moves.
    pub fn moved(self, event: &PointerEvent) {
        let captured = self.scene.with_value(|s| s.captured.get_untracked());
        if captured && event.is_trusted() {
            return;
        }
        let Some((start_x, start_y)) = self.anchor.get_value() else {
            return;
        };
        let (vw, vh) = viewport();
        let cur_x = event.client_x().clamp(0.0, vw);
        let cur_y = event.client_y().clamp(0.0, vh);

        let width = (cur_x - start_x).abs();
        let height = (cur_y - start_y).abs();

        // 3px deadband so normal clicks don't flicker a box
        if width > 3.0 || height > 3.0 {
            let x = start_x.min(cur_x);
            let y = start_y.min(cur_y);
            self.rect.set(Some(MarqueeRect {
                x,
                y,
                width,
                height,
            }));
        } else {
            self.rect.set(None);
        }
    }

    /// Finish the marquee selection gesture.
    pub fn up(self, event: &PointerEvent) {
        let captured = self.scene.with_value(|s| s.captured.get_untracked());
        if captured && event.is_trusted() {
            return;
        }
        if self.anchor.get_value().is_some() {
            if let Some(desktop) = self.desktop_ref.get() {
                let _ = desktop.release_pointer_capture(event.pointer_id());
            }
            self.pointer_id.set_value(None);
            self.anchor.set_value(None);
            self.rect.set(None);
        }
    }

    /// Cancel the marquee selection gesture.
    pub fn cancel(self) {
        if self.anchor.get_value().is_some() {
            if let Some(desktop) = self.desktop_ref.get()
                && let Some(pid) = self.pointer_id.get_value()
            {
                let _ = desktop.release_pointer_capture(pid);
            }
            self.pointer_id.set_value(None);
            self.anchor.set_value(None);
            self.rect.set(None);
        }
    }

    /// Listen for `Escape` on the window to immediately cancel an active selection drag.
    fn watch_escape(self) {
        let listener = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            if event.key() == "Escape" && self.is_active() {
                event.prevent_default();
                self.cancel();
            }
        });
        if let Some(window) = web_sys::window() {
            let _ = window
                .add_event_listener_with_callback("keydown", listener.as_ref().unchecked_ref());
        }
        listener.forget();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marquee_intersects_overlapping() {
        let a = MarqueeRect {
            x: 10.0,
            y: 10.0,
            width: 50.0,
            height: 50.0,
        };
        let b = MarqueeRect {
            x: 30.0,
            y: 30.0,
            width: 20.0,
            height: 20.0,
        };
        assert!(a.intersects(&b));
        assert!(b.intersects(&a));
    }

    #[test]
    fn marquee_intersects_disjoint() {
        let a = MarqueeRect {
            x: 10.0,
            y: 10.0,
            width: 50.0,
            height: 50.0,
        };
        let b = MarqueeRect {
            x: 100.0,
            y: 100.0,
            width: 20.0,
            height: 20.0,
        };
        assert!(!a.intersects(&b));
        assert!(!b.intersects(&a));
    }

    #[test]
    fn marquee_intersects_touching_edges() {
        let a = MarqueeRect {
            x: 10.0,
            y: 10.0,
            width: 50.0,
            height: 50.0,
        };
        let b = MarqueeRect {
            x: 60.0,
            y: 10.0,
            width: 20.0,
            height: 50.0,
        };
        assert!(!a.intersects(&b));
    }

    #[test]
    fn marquee_contains_point() {
        let r = MarqueeRect {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        assert!(r.contains_point(10.0, 20.0));
        assert!(r.contains_point(50.0, 40.0));
        assert!(r.contains_point(110.0, 70.0));
        assert!(!r.contains_point(9.0, 20.0));
        assert!(!r.contains_point(50.0, 71.0));
    }
}
