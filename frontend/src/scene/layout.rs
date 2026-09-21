//! Everything the shell changes about a window on its own: stacking, position,
//! minimize, maximize, snap, workspace. None of it reaches the compositor.

use std::sync::atomic::{AtomicU64, Ordering};

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use webland_core::SurfaceId;

use super::{Scene, SnapZone, Toast};

impl Scene {
    /// Put a window above the others. Browser state; the compositor is not told.
    pub fn raise(&self, id: SurfaceId) {
        self.top.set(self.top.get() + 1);
        let top = self.top.get();
        self.windows.update(|ws| {
            if let Some(window) = ws.iter_mut().find(|w| w.id == id.0) {
                window.z = top;
            }
        });
        self.focused.set(Some(id.0));
    }

    /// Move a window to a new top-left corner.
    pub fn move_to(&self, id: u64, x: i32, y: i32) {
        self.windows.update(|ws| {
            if let Some(window) = ws.iter_mut().find(|w| w.id == id) {
                window.x = x;
                window.y = y;
            }
        });
    }

    /// Hide a window, or bring it back. Browser state; the client goes on
    /// drawing, and never learns it is not being looked at.
    pub fn set_minimized(&self, id: u64, minimized: bool) {
        self.windows.update(|ws| {
            if let Some(window) = ws.iter_mut().find(|w| w.id == id) {
                window.minimized = minimized;
            }
        });
    }

    /// Send a window to the corner, or back where it came from.
    ///
    /// Only the corner: the size is the client's answer to the configure the
    /// caller sends, and arrives later as a fresh `SurfaceCreated`.
    pub fn set_maximized(&self, id: u64, maximized: bool) {
        self.windows.update(|ws| {
            let Some(window) = ws.iter_mut().find(|w| w.id == id) else {
                return;
            };
            match (maximized, window.restore) {
                (true, None) => {
                    if window.floating.is_none() {
                        window.floating = Some((window.x, window.y, window.width, window.height));
                    }
                    window.restore = Some((window.x, window.y));
                    window.snap = Some(SnapZone::Maximize);
                    window.x = 0;
                    window.y = 0;
                }
                (false, _) => {
                    if let Some((x, y, w, h)) = window.floating.take() {
                        window.x = x;
                        window.y = y;
                        window.width = w;
                        window.height = h;
                    } else if let Some((x, y)) = window.restore {
                        window.x = x;
                        window.y = y;
                    }
                    window.restore = None;
                    window.snap = None;
                }
                _ => {}
            }
        });
    }

    /// Snap a window to a half-screen or maximized.
    pub fn snap_to(&self, id: u64, zone: SnapZone, width: u32, height: u32, x: i32, y: i32) {
        self.windows.update(|ws| {
            let Some(window) = ws.iter_mut().find(|w| w.id == id) else {
                return;
            };
            if window.floating.is_none() {
                window.floating = Some((window.x, window.y, window.width, window.height));
            }
            window.snap = Some(zone);
            if zone == SnapZone::Maximize {
                window.restore = Some((window.x, window.y));
            }
            window.width = width;
            window.height = height;
            window.x = x;
            window.y = y;
        });
    }

    /// Un-snap or un-maximize a window back to its floating dimensions.
    pub fn unsnap(&self, id: u64) -> Option<(i32, i32, u32, u32)> {
        let mut restored = None;
        self.windows.update(|ws| {
            let Some(window) = ws.iter_mut().find(|w| w.id == id) else {
                return;
            };
            if let Some((x, y, w, h)) = window.floating.take() {
                window.x = x;
                window.y = y;
                window.width = w;
                window.height = h;
                restored = Some((x, y, w, h));
            } else if let Some((x, y)) = window.restore.take() {
                window.x = x;
                window.y = y;
                restored = Some((x, y, window.width, window.height));
            }
            window.snap = None;
            window.restore = None;
        });
        restored
    }

    /// Whether a window is currently snapped to half or maximized.
    #[must_use]
    pub fn is_snapped(&self, id: u64) -> bool {
        self.windows.with_untracked(|ws| {
            ws.iter()
                .find(|w| w.id == id)
                .is_some_and(|w| w.snap.is_some() || w.restore.is_some())
        })
    }

    /// Send a window to another workspace, and follow it there.
    ///
    /// Following matters: a window that vanishes because it was dropped
    /// somewhere the user is not looking reads as having been closed.
    pub fn send_to_workspace(&self, id: u64, workspace: u32) {
        self.windows.update(|ws| {
            if let Some(window) = ws.iter_mut().find(|w| w.id == id) {
                window.workspace = workspace;
                // A window arriving somewhere hidden is not what "send this
                // there" means.
                window.minimized = false;
            }
        });
        self.workspace.set(workspace);
    }

    /// Set a window's box and position while a resize handle is being dragged.
    ///
    /// Only the box and origin. The canvas bitmap keeps the size the client last
    /// rendered at, so CSS stretches the last frame for the length of the gesture;
    /// the client is told once, on release, and the sharp redraw comes back as a
    /// fresh `SurfaceCreated`.
    pub fn resize_and_move_to(&self, id: u64, width: u32, height: u32, x: i32, y: i32) {
        self.windows.update(|ws| {
            if let Some(window) = ws.iter_mut().find(|w| w.id == id) {
                window.width = width;
                window.height = height;
                window.x = x;
                window.y = y;
            }
        });
    }

    /// Whether a window is currently maximized.
    #[must_use]
    pub fn is_maximized(&self, id: u64) -> bool {
        self.windows.with_untracked(|ws| {
            ws.iter()
                .any(|w| w.id == id && (w.snap == Some(SnapZone::Maximize) || w.restore.is_some()))
        })
    }

    /// Whether this surface is one the scene already holds a window for.
    ///
    /// Tells a surface being announced for the first time from one being
    /// re-announced after a resize, which the wire cannot: both arrive as
    /// `SurfaceCreated`.
    #[must_use]
    pub fn knows(&self, id: SurfaceId) -> bool {
        self.windows.with_untracked(|ws| ws.iter().any(|w| w.id == id.0))
    }

    /// Whether a surface is currently visible on screen (not minimized and on the active workspace).
    /// Follows child popups/anchors up to their root parent window.
    #[must_use]
    pub fn is_visible(&self, mut id: SurfaceId) -> bool {
        let current_ws = self.workspace.get_untracked();
        self.windows.with_untracked(|ws| {
            for _ in 0..10 {
                if let Some(w) = ws.iter().find(|w| w.id == id.0) {
                    if w.minimized || w.workspace != current_ws {
                        return false;
                    }
                    if let Some(anchor) = w.parent {
                        id = anchor.parent;
                    } else {
                        return true;
                    }
                } else {
                    return true;
                }
            }
            true
        })
    }

    /// Push a system toast notification that auto-dismisses after 3.5 seconds.
    pub fn show_toast(&self, title: impl Into<String>, message: Option<String>) {
        // Counted, not clocked. Two toasts raised in the same millisecond (
        // which is what a keyboard shortcut that raises two of them does) took
        // the same wall-clock id, and `<For>` keys by id: the second replaced
        // the first, and the first one's dismissal timer then took both.
        static NEXT_TOAST: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_TOAST.fetch_add(1, Ordering::Relaxed);
        let toast = Toast {
            id,
            title: title.into(),
            message,
        };
        self.toasts.update(|ts| ts.push(toast));
        let toasts = self.toasts;
        if let Some(window) = web_sys::window() {
            let listener = Closure::<dyn FnMut()>::new(move || {
                toasts.update(|ts| ts.retain(|t| t.id != id));
            });
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                listener.as_ref().unchecked_ref(),
                3500,
            );
            listener.forget();
        }
    }

    /// The size a window is currently drawn at, for resize arithmetic.
    #[must_use]
    pub fn window_size_of(&self, id: u64) -> (u32, u32) {
        self.windows
            .with_untracked(|ws| ws.iter().find(|w| w.id == id).map(|w| (w.width, w.height)))
            .unwrap_or((1, 1))
    }

    /// A window's current top-left corner, for drag arithmetic.
    #[must_use]
    pub fn window_origin(&self, id: u64) -> (i32, i32) {
        self.windows
            .with_untracked(|ws| ws.iter().find(|w| w.id == id).map(|w| (w.x, w.y)))
            .unwrap_or((0, 0))
    }
}
