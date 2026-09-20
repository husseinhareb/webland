//! What the shell draws for each surface, and the two transient bits of chrome
//! that hang off the window list: the Alt+Tab switcher and the toast stack.

use webland_core::Rect;
use webland_protocol::Anchor;

use super::SnapZone;

/// Active state for the Alt+Tab window switcher modal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AltTabState {
    pub selected_index: usize,
    pub window_ids: Vec<u64>,
}

/// A desktop system toast notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub id: u64,
    pub title: String,
    pub message: Option<String>,
}

/// Everything about a window that the shell draws, and nothing the compositor
/// needs to know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowState {
    pub id: u64,
    /// The window's box on screen, in device pixels. Starts as the client's own
    /// window size and runs ahead of it for the length of a resize drag.
    pub width: u32,
    pub height: u32,
    /// The streamed image's size, which is the canvas bitmap's size, bigger
    /// than the window whenever the client drew a shadow around it.
    pub image: (u32, u32),
    /// Where the window sits inside that image. Everything outside it is the
    /// client's own margin, which is black once encoded and so is clipped away.
    pub content: Rect,
    pub title: String,
    /// What the client calls itself (`firefox`, `org.gnome.Nautilus`) which
    /// is how the panel finds the application's icon. `None` until the client
    /// says, and for a client that never does.
    pub app_id: Option<String>,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    /// Hidden, but still open and still streaming. The panel is how it comes
    /// back, which is why a minimized window keeps its task button.
    pub minimized: bool,
    /// Which workspace the window sits on. Browser state like position and
    /// stacking: switching workspaces shows and hides windows and tells the
    /// compositor nothing.
    pub workspace: u32,
    /// Set when this is a popup (a menu or a tooltip) which is not a window:
    /// it has no chrome, no task button and no position of its own, and hangs
    /// off the surface that opened it until that surface goes or it is
    /// dismissed.
    pub parent: Option<Anchor>,
    /// Whether the shell draws this window's chrome. False for a client that
    /// drew its own titlebar, where a second one would sit directly above it.
    pub decorated: bool,
    /// Where the window sat before it was maximized, so `Some` is what it
    /// means to be maximized, and there is no way to be maximized with nowhere
    /// to go back to.
    pub restore: Option<(i32, i32)>,
    /// Whether the surface has an active pointer lock constraint.
    pub pointer_locked: bool,
    /// Which snap zone the window is currently tiled to, if any.
    pub snap: Option<SnapZone>,
    /// Floating geometry prior to snapping/maximizing: (x, y, width, height)
    pub floating: Option<(i32, i32, u32, u32)>,
}
