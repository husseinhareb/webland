//! The host's cursor theme, read off disk for the browser to draw with.
//!
//! The pointer is the browser's own everywhere except one place: while a client
//! holds the pointer, the page has it locked and the shell draws a cursor of its
//! own, which has to come from somewhere. It comes from here, so the desktop's
//! pointer is the one the host session uses rather than something drawn by hand.

use std::str::FromStr;
use std::sync::OnceLock;

use smithay::input::pointer::CursorIcon;
use webland_protocol::CursorShape;
use xcursor::CursorTheme;
use xcursor::parser::{Image, parse_xcursor};

/// The shapes the shell can put on screen, which is what the frontend's
/// `element_cursor_icon` chooses between. Anything else a client asks for is
/// named on the wire and drawn by the browser from the keyword alone.
const SHAPES: &[&str] = &[
    "default",
    "pointer",
    "text",
    "grab",
    "grabbing",
    "ns-resize",
    "ew-resize",
    "nwse-resize",
    "nesw-resize",
];

/// The size to ask the theme for, when `XCURSOR_SIZE` says nothing.
const DEFAULT_SIZE: u32 = 24;

/// Every shape the shell draws, in the host's theme.
///
/// Read once: a theme does not change under a running session, and a shape that
/// the theme does not carry is simply absent, leaving the browser its keyword.
pub fn theme() -> &'static [CursorShape] {
    static SHAPES_LOADED: OnceLock<Vec<CursorShape>> = OnceLock::new();
    SHAPES_LOADED.get_or_init(load)
}

fn load() -> Vec<CursorShape> {
    let name = env("XCURSOR_THEME").unwrap_or_else(|| String::from("default"));
    let size = env("XCURSOR_SIZE")
        .and_then(|value| value.parse().ok())
        .filter(|size| *size > 0)
        .unwrap_or(DEFAULT_SIZE);
    let theme = CursorTheme::load(&name);
    let shapes: Vec<CursorShape> = SHAPES
        .iter()
        .filter_map(|shape| read(&theme, shape, size))
        .collect();
    if shapes.is_empty() {
        tracing::warn!(theme = %name, "no cursor theme on the host; the shell draws its own pointer");
    } else {
        tracing::info!(theme = %name, size, shapes = shapes.len(), "cursor theme");
    }
    shapes
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// One shape, at the size closest to the one asked for.
fn read(theme: &CursorTheme, shape: &str, size: u32) -> Option<CursorShape> {
    // The XDG name is the CSS one, and a theme that predates those names carries
    // the shape under an X11 name instead: `left_ptr` for `default`, `xterm` for
    // `text`. `CursorIcon` knows both, so the lookup tries the alternatives
    // before giving up on a shape.
    let icon = CursorIcon::from_str(shape).ok();
    let alternatives: &[&str] = icon.as_ref().map_or(&[], |icon| icon.alt_names());
    let path = std::iter::once(shape)
        .chain(alternatives.iter().copied())
        .find_map(|name| theme.load_icon(name))?;
    let images = parse_xcursor(&std::fs::read(path).ok()?)?;
    // An animated cursor repeats its size once per frame; the nearest match is
    // therefore the first frame of it, which is the one that gets drawn.
    let image = images.iter().min_by_key(|image| image.size.abs_diff(size))?;
    Some(CursorShape {
        name: shape.to_string(),
        width: image.width,
        height: image.height,
        hotspot_x: image.xhot,
        hotspot_y: image.yhot,
        rgba: straight(image),
    })
}

/// A cursor's pixels with the alpha divided back out.
///
/// Cursor files hold premultiplied alpha, which is what a compositor's renderer
/// wants and the opposite of what `ImageData` does: handed premultiplied pixels
/// it darkens every soft edge, drawing a black fringe around the pointer.
fn straight(image: &Image) -> Vec<u8> {
    image
        .pixels_rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| {
            let alpha = pixel[3];
            if alpha == 0 || alpha == 255 {
                return [pixel[0], pixel[1], pixel[2], alpha];
            }
            let divide = |channel: u8| {
                u8::try_from((u32::from(channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                    .unwrap_or(u8::MAX)
            };
            [divide(pixel[0]), divide(pixel[1]), divide(pixel[2]), alpha]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Undoing premultiplication is what keeps a soft edge soft: a half
    /// transparent white pixel is stored halved, and has to come back white.
    #[test]
    fn premultiplied_pixels_come_back_straight() {
        let image = Image {
            size: 24,
            width: 1,
            height: 2,
            xhot: 0,
            yhot: 0,
            delay: 0,
            pixels_argb: Vec::new(),
            pixels_rgba: vec![128, 128, 128, 128, 255, 0, 0, 255],
        };
        // The half-transparent grey was white before it was premultiplied; the
        // opaque red is already straight and must come back untouched.
        assert_eq!(straight(&image), vec![255, 255, 255, 128, 255, 0, 0, 255]);
    }
}

