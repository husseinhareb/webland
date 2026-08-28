//! Shared types and errors for the Webland backend.
//!
//! The vocabulary (ids, geometry, errors) shared by the other backend crates
//! and, because the frontend is Rust too, by the browser side via
//! `webland-protocol`. Defined once here so both ends speak the same units.

use serde::{Deserialize, Serialize};

/// Identifies a surface for the lifetime of a connection.
///
/// Allocated by the compositor; the browser never invents one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SurfaceId(pub u64);

/// Pixel dimensions of a surface or output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

/// A position in surface-local pixels. Floating point because Wayland pointer
/// coordinates are sub-pixel (`wl_fixed`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// An axis-aligned rectangle in surface-local pixels, used for damage regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Errors surfaced by the Webland backend.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Placeholder variant; replaced as real failure modes appear.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Convenience alias used across the backend crates.
pub type Result<T> = std::result::Result<T, Error>;
