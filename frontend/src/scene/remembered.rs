//! Where the windows were, so a reload does not scatter them.
//!
//! The scene is browser-side state, position, workspace, whether a window is
//! minimized, and the compositor neither knows nor can be asked. Reloading the
//! page therefore threw all of it away and re-cascaded every surface from the
//! top left, which is the one thing that made the desktop feel like a web page.
//!
//! `sessionStorage`, not `localStorage`: this belongs to the tab showing the
//! desktop, and a tab opened tomorrow against a compositor that has been
//! restarted should not inherit the placements of windows that no longer exist.
//!
//! ponytail: position, workspace and minimized only. Snapping and maximizing
//! are a size the compositor was told about too, so restoring them means
//! re-sending it; remember them when a half-tiled window coming back floating
//! turns out to matter.

use std::collections::HashMap;

use super::WindowState;

/// Where one window sat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub workspace: u32,
    pub minimized: bool,
}

const KEY: &str = "webland.layout";

/// Read back what the last load of this tab left.
#[must_use]
pub fn load() -> HashMap<u64, Placement> {
    storage()
        .and_then(|storage| storage.get_item(KEY).ok().flatten())
        .map(|text| parse(text.as_str()))
        .unwrap_or_default()
}

/// Remember where the windows are now.
///
/// Popups are left out: they are placed by their client against a parent, and
/// none of them outlives a reload.
pub fn save(windows: &[WindowState]) {
    let Some(storage) = storage() else {
        return;
    };
    let _ = storage.set_item(KEY, &format(windows));
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.session_storage().ok().flatten()
}

/// `id,x,y,workspace,minimized;…`; small enough not to want a JSON parser.
fn format(windows: &[WindowState]) -> String {
    windows
        .iter()
        .filter(|window| window.parent.is_none())
        .map(|window| {
            format!(
                "{},{},{},{},{}",
                window.id,
                window.x,
                window.y,
                window.workspace,
                u8::from(window.minimized)
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Anything malformed is dropped rather than guessed at: a window in the wrong
/// place is worse than a window in the default one.
fn parse(text: &str) -> HashMap<u64, Placement> {
    text.split(';')
        .filter_map(|entry| {
            let mut fields = entry.split(',');
            let id = fields.next()?.parse().ok()?;
            let placement = Placement {
                x: fields.next()?.parse().ok()?,
                y: fields.next()?.parse().ok()?,
                workspace: fields.next()?.parse().ok()?,
                minimized: fields.next()? == "1",
            };
            Some((id, placement))
        })
        .collect()
}
