//! Wayland compositor for Webland.
//!
//! Built on [`smithay`]. Wayland-first: `XWayland` support, if it ever lands,
//! goes behind a feature flag rather than into this module.
//!
//! No compositor state, globals, or event loop yet.

/// Re-exported so downstream crates pin one Wayland stack.
pub use smithay;
