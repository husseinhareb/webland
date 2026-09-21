//! The browser-side scene: one window per Wayland surface.
//!
//! Phase 4 proved the streaming is per-surface; this is where that becomes a
//! desktop. Each surface has its own canvas, renderer and decoder; a separate
//! decoder is not a nicety, since each surface is an independent H.264 stream
//! with its own keyframes, and feeding two of them to one decoder produces
//! garbage from the first frame.
//!
//! Position, size and stacking live in [`WindowState`], which is a Leptos
//! signal: moving or raising a window rerenders a style attribute and tells the
//! compositor nothing at all.

mod geometry;
mod gesture;
mod layout;
mod remembered;
mod state;
mod window;

pub use geometry::{
    ActiveResize, Grab, MIN_SURFACE, ResizeDirection, SnapZone, coord, desktop_bounds, drag_origin,
    element_at, maximized_size, pixel_ratio, pixels, whole_blocks, workspace_at,
};
pub use state::{CursorArt, Scene};
pub use window::{AltTabState, Toast, WindowState};
