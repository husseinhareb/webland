//! Where a window and its canvas are drawn, computed from the scene's signal.
//!
//! Read through the signal rather than a prop. `<For>` is keyed by id, so a
//! window whose title or position changes is not rebuilt; a prop is a snapshot
//! from the moment the row first appeared, and a static style attribute would
//! leave dragging visibly doing nothing.

use leptos::prelude::*;

use crate::scene::{Scene, WindowState, pixel_ratio};

/// The window's current state, or `None` once it has been destroyed.
pub fn state(scene: StoredValue<Scene, LocalStorage>, id: u64) -> Option<WindowState> {
    scene.with_value(|scene| {
        scene
            .windows
            .with(|ws| ws.iter().find(|w| w.id == id).cloned())
    })
}

pub fn title(scene: StoredValue<Scene, LocalStorage>, id: u64) -> String {
    state(scene, id).map(|w| w.title).unwrap_or_default()
}

/// The window frame's box.
///
/// Hidden with `display`, never unmounted: tearing the row down would take the
/// canvas with it and leave the renderer drawing into a detached one, which
/// succeeds and shows nothing ever after. Another workspace hides a window
/// exactly as minimizing does, and for the same reason.
///
/// ponytail: a hidden window goes on streaming, and off-screen surfaces are the
/// encode cost the roadmap's risk table flags. Tell the compositor to stop
/// sending them if window count starts to hurt.
pub fn frame(scene: StoredValue<Scene, LocalStorage>, id: u64, workspace: u32) -> String {
    let Some(window) = state(scene, id) else {
        return String::new();
    };
    let ratio = pixel_ratio();
    // A popup hangs off the window that opened it: placed where that window is
    // now, not where it was when the client asked, and hidden whenever its
    // parent is. Read reactively, so dragging the parent drags the menu with it.
    if let Some(anchor) = window.parent
        && let Some(parent) = state(scene, anchor.parent.0)
    {
        return format!(
            "left:{}px; top:{}px; z-index:{}; width:{}px; {}",
            f64::from(parent.x) + f64::from(anchor.x) / ratio,
            f64::from(parent.y) + f64::from(anchor.y) / ratio,
            parent.z + 1,
            f64::from(window.width) / ratio,
            hidden(&parent, workspace),
        );
    }
    format!(
        "left:{}px; top:{}px; z-index:{}; width:{}px; {}",
        window.x,
        window.y,
        window.z,
        f64::from(window.width) / ratio,
        hidden(&window, workspace),
    )
}

fn hidden(window: &WindowState, workspace: u32) -> &'static str {
    if window.minimized || window.workspace != workspace {
        "display:none;"
    } else {
        ""
    }
}

/// The box the canvas sits in, which is the window.
pub fn surface(scene: StoredValue<Scene, LocalStorage>, id: u64) -> String {
    state(scene, id).map_or_else(String::new, |w| {
        let ratio = pixel_ratio();
        format!(
            "width:{}px; height:{}px;",
            f64::from(w.width) / ratio,
            f64::from(w.height) / ratio,
        )
    })
}

/// The canvas itself, which is the whole streamed image, bigger than the window
/// for a client that drew a shadow around itself, and offset so the window's
/// corner lands in the corner.
///
/// A resize drag scales both, stretching the last frame until the client answers
/// at the new size: the same stretch, applied to a picture that is now only
/// partly on show.
pub fn canvas(scene: StoredValue<Scene, LocalStorage>, id: u64, cursor: &str) -> String {
    state(scene, id).map_or_else(String::new, |w| {
        let ratio = pixel_ratio();
        let sx = f64::from(w.width) / f64::from(w.content.width.max(1));
        let sy = f64::from(w.height) / f64::from(w.content.height.max(1));
        let (image_w, image_h) = w.image;
        // The cursor rides on the canvas rather than the desktop, so it is the
        // client's over a client's pixels and the shell's everywhere else; the
        // titlebar's grab hand, the resize handles' arrows.
        format!(
            "cursor:{cursor}; width:{}px; height:{}px; left:{}px; top:{}px;",
            f64::from(image_w) * sx / ratio,
            f64::from(image_h) * sy / ratio,
            -f64::from(w.content.x) * sx / ratio,
            -f64::from(w.content.y) * sy / ratio,
        )
    })
}
