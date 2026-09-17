//! Desktop geometry: the viewport, the snap zones, and the two gestures that
//! change a window's box.

use webland_core::Size;

/// Round a float to a pixel count.
///
/// The clamp is the point. `as` alone is saturating, so it cannot produce a
/// wrapped value — but it turns a negative into 0 and a NaN into 0 without
/// saying so, and a zero-sized window is not a thing a caller ever wants. One
/// pixel is: it is visibly wrong instead of invisibly wrong.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped above
pub fn pixels(value: f64) -> u32 {
    if value.is_nan() {
        return 1;
    }
    value.round().clamp(1.0, f64::from(u32::MAX)) as u32
}

/// Round a float to a screen coordinate.
///
/// Same reasoning as [`pixels`], except that a coordinate is allowed to be
/// negative — a window dragged off the left edge has a negative `x` — so only
/// the range is clamped.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // clamped above
pub fn coord(value: f64) -> i32 {
    if value.is_nan() {
        return 0;
    }
    value
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// The element at a viewport coordinate.
///
/// `elementFromPoint` takes `f32`; the narrowing is lossless at any coordinate a
/// screen actually has.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // f64 -> f32, at screen scale
pub fn element_at(document: &web_sys::Document, x: f64, y: f64) -> Option<web_sys::Element> {
    document.element_from_point(x as f32, y as f32)
}

/// The browser's device pixel ratio, never zero.
#[must_use]
pub fn pixel_ratio() -> f64 {
    let ratio = web_sys::window().map_or(1.0, |window| window.device_pixel_ratio());
    if ratio > 0.0 { ratio } else { 1.0 }
}

/// The usable bounds of the desktop viewport in CSS pixels (width, height),
/// subtracting the panel height.
#[must_use]
pub fn desktop_bounds() -> (f64, f64) {
    let window = web_sys::window();
    let width = window
        .as_ref()
        .and_then(|w| w.inner_width().ok())
        .and_then(|v| v.as_f64())
        .unwrap_or(1920.0);
    let panel = window
        .as_ref()
        .and_then(web_sys::Window::document)
        .and_then(|document| document.query_selector("#webland-panel").ok().flatten())
        .map_or(40.0, |p| f64::from(p.client_height()));
    let height = window
        .as_ref()
        .and_then(|w| w.inner_height().ok())
        .and_then(|v| v.as_f64())
        .unwrap_or(1080.0)
        - panel;
    (width, height.max(200.0))
}

/// The whole desktop, in device pixels: everything the panel has not taken.
#[must_use]
pub fn maximized_size() -> Size {
    let (width, height) = desktop_bounds();
    let ratio = pixel_ratio();
    Size {
        width: pixels(width * ratio),
        height: pixels(height * ratio),
    }
}

/// A window's corner during a drag: the pointer, less the grab offset, never
/// above the top of the screen — a titlebar dragged off it cannot be grabbed
/// again.
#[must_use]
pub fn drag_origin((cx, cy): (f64, f64), (dx, dy): (f64, f64)) -> (i32, i32) {
    (coord(cx - dx), coord(cy - dy).max(0))
}

/// The workspace button under a coordinate (x, y), if any.
#[must_use]
pub fn workspace_at(x: f64, y: f64) -> Option<u32> {
    let document = web_sys::window()?.document()?;
    element_at(&document, x, y)?
        .closest("[data-workspace]")
        .ok()
        .flatten()?
        .get_attribute("data-workspace")?
        .parse()
        .ok()
}

/// The smallest a window may be dragged, in device pixels. Small enough to be
/// no real limit, large enough that a window can never lose its own grip.
pub const MIN_SURFACE: f64 = 160.0;

/// Where a window can snap to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SnapZone {
    Maximize,
    Left,
    Right,
}

/// Which edge or corner is being dragged during a resize.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ResizeDirection {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl ResizeDirection {
    /// The CSS cursor keyword for dragging this edge or corner.
    #[must_use]
    pub fn cursor(self) -> &'static str {
        match self {
            Self::Top | Self::Bottom => "ns-resize",
            Self::Left | Self::Right => "ew-resize",
            Self::TopLeft | Self::BottomRight => "nwse-resize",
            Self::TopRight | Self::BottomLeft => "nesw-resize",
        }
    }
}

/// Where a resize began: the edge/corner grabbed, the pointer's coordinates,
/// and the window's origin and size at the moment the gesture started.
#[derive(Copy, Clone, Debug)]
pub struct ActiveResize {
    pub dir: ResizeDirection,
    pub from_x: f64,
    pub from_y: f64,
    pub orig_x: i32,
    pub orig_y: i32,
    pub orig_width: u32,
    pub orig_height: u32,
}

/// Where a move began: the pointer's offset into the titlebar, and the corner
/// the window started from: (dx, dy, `orig_x`, `orig_y`).
pub type Grab = (f64, f64, i32, i32);
