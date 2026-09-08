//! What the launcher can start.
//!
//! Read from freedesktop `.desktop` files, which is where every installed
//! application already describes itself — name, command, and whether it wants to
//! be shown in a menu at all. Nothing here is configured by hand.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use webland_protocol::Application;

/// The applications on this machine, and how to start each one.
#[derive(Debug, Default)]
pub struct Applications {
    /// Ordered for display; the index is the id the browser sends back.
    names: Vec<Application>,
    commands: HashMap<u32, String>,
}

impl Applications {
    /// Scan the usual directories.
    #[must_use]
    pub fn scan() -> Self {
        let mut found: Vec<(String, String, Option<String>)> = Vec::new();
        let home = std::env::var("HOME").unwrap_or_default();
        let dirs = [
            format!("{home}/.local/share/applications"),
            String::from("/usr/share/applications"),
        ];
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "desktop")
                    && let Some(app) = read_entry(&path)
                {
                    found.push(app);
                }
            }
        }
        // Stable, and by name: the launcher is a list a person reads.
        found.sort_by_key(|(name, _, _)| name.to_lowercase());
        found.dedup_by(|a, b| a.0 == b.0);

        let mut names = Vec::with_capacity(found.len());
        let mut commands = HashMap::with_capacity(found.len());
        for (index, (name, exec, icon)) in found.into_iter().enumerate() {
            let Ok(id) = u32::try_from(index) else { break };
            names.push(Application {
                id,
                name,
                icon: icon.as_deref().and_then(icon_data_url),
            });
            commands.insert(id, exec);
        }
        Self { names, commands }
    }

    /// The list to show, in display order.
    #[must_use]
    pub fn listing(&self) -> Vec<Application> {
        self.names.clone()
    }

    /// Start one, on the given Wayland display.
    ///
    /// The id must have come from [`Applications::listing`]; an unknown one is
    /// ignored rather than guessed at.
    pub fn launch(&self, id: u32, display: &std::ffi::OsStr) {
        let Some(command) = self.commands.get(&id) else {
            tracing::warn!(id, "launch request for an unknown application");
            return;
        };
        let mut parts = command.split_whitespace();
        let Some(program) = parts.next() else {
            return;
        };
        match std::process::Command::new(program)
            .args(parts)
            .env("WAYLAND_DISPLAY", display)
            .spawn()
        {
            Ok(_) => tracing::info!(%command, "launched"),
            Err(err) => tracing::warn!(%command, %err, "could not launch"),
        }
    }
}

/// Pull the name and command out of one `.desktop` file.
///
/// Returns `None` for anything that should not appear in a menu: entries that
/// are not applications, ones marked `NoDisplay`, and ones needing a terminal —
/// which would want a terminal emulator wrapped around them, and there is no
/// sensible one to pick from here.
fn read_entry(path: &Path) -> Option<(String, String, Option<String>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut exec = None;
    let mut icon = None;
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        match line.split_once('=') {
            Some(("Name", value)) if name.is_none() => name = Some(value.trim().to_string()),
            Some(("Exec", value)) if exec.is_none() => exec = Some(value.trim().to_string()),
            Some(("Icon", value)) if icon.is_none() => icon = Some(value.trim().to_string()),
            Some(("NoDisplay" | "Hidden" | "Terminal", "true")) => return None,
            Some(("Type", value)) if value.trim() != "Application" => return None,
            _ => {}
        }
    }
    let exec = strip_field_codes(&exec?);
    let name = name?;
    (!name.is_empty() && !exec.is_empty()).then_some((name, exec, icon))
}

/// Where an icon name resolves to a file a browser can render.
///
/// `Icon=` is either an absolute path or a name to look up under the icon
/// theme directories.
///
/// ponytail: `hicolor` and `pixmaps` only — no `index.theme` parsing, so an
/// icon that exists solely in the user's chosen theme is missed. hicolor is the
/// spec's fallback and where applications install themselves, which covers
/// nearly all of them; read the theme when one turns up missing.
fn icon_path(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') {
        let path = PathBuf::from(name);
        return path.is_file().then_some(path);
    }
    // A name with a slash in it would climb out of the icon directories.
    if name.contains('/') || name.is_empty() {
        return None;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let roots = [
        format!("{home}/.local/share/icons"),
        format!("{home}/.icons"),
        String::from("/usr/share/icons"),
    ];
    for root in &roots {
        let hicolor = PathBuf::from(format!("{root}/hicolor"));
        // Read the sizes rather than guess them — applications install at
        // whatever size they please, 512 as readily as 48 — and take the
        // smallest that is still crisp: the panel draws these about 20px, where
        // a 48px png costs a few KB and the scalable svg can cost a hundred.
        let mut sizes: Vec<u32> = std::fs::read_dir(&hicolor)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.split_once('x')?.0.parse().ok())
            .filter(|size| *size >= 48)
            .collect();
        sizes.sort_unstable();
        for size in sizes {
            let path = hicolor.join(format!("{size}x{size}/apps/{name}.png"));
            if path.is_file() {
                return Some(path);
            }
        }
        let scalable = hicolor.join(format!("scalable/apps/{name}.svg"));
        if scalable.is_file() {
            return Some(scalable);
        }
    }
    for ext in ["svg", "png"] {
        let path = PathBuf::from(format!("/usr/share/pixmaps/{name}.{ext}"));
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// An icon as a `data:` URL, ready to hang on an `<img>`.
///
/// Inlined rather than fetched: the compositor speaks one WebSocket and serves
/// no HTTP, and the whole listing is one message sent once.
fn icon_data_url(name: &str) -> Option<String> {
    let path = icon_path(name)?;
    let bytes = std::fs::read(&path).ok()?;
    // The listing goes over the wire in a single message, so one oversized icon
    // would be paid for by every browser that connects. None is better.
    //
    // ponytail: taken at whatever size it was installed, since downscaling means
    // a PNG decoder. An application that ships one huge icon and no smaller one
    // (VS Code, 220 KiB) is most of what the listing costs; decode and downscale
    // if that ever matters.
    if bytes.is_empty() || bytes.len() > 256 * 1024 {
        return None;
    }
    let mime = if path.extension().is_some_and(|ext| ext == "svg") {
        "image/svg+xml"
    } else {
        "image/png"
    };
    Some(format!("data:{mime};base64,{}", base64(&bytes)))
}

/// Standard base64, which is all the `data:` URL above needs.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let bits = u32::from(chunk[0]) << 16
            | u32::from(chunk.get(1).copied().unwrap_or(0)) << 8
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for slot in 0..4 {
            if slot <= chunk.len() {
                out.push(char::from(
                    ALPHABET[(bits >> (18 - 6 * slot)) as usize & 63],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Drop the `%f`, `%U`, … placeholders a `.desktop` Exec line may carry.
///
/// They stand for files and URLs passed to the program; there are none here, and
/// passing them through literally would have the application open a file called
/// `%U`.
fn strip_field_codes(exec: &str) -> String {
    exec.split_whitespace()
        .filter(|word| !(word.len() == 2 && word.starts_with('%')))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{base64, icon_path, strip_field_codes};

    #[test]
    fn field_codes_are_dropped_but_arguments_are_not() {
        assert_eq!(strip_field_codes("firefox %u"), "firefox");
        assert_eq!(strip_field_codes("kitty -e fish %F"), "kitty -e fish");
        // A percent that is not a field code is just an argument.
        assert_eq!(strip_field_codes("app --pct 50%"), "app --pct 50%");
    }

    #[test]
    fn base64_pads_every_remainder() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // All 64 symbols, and the high bit set.
        assert_eq!(base64(&[0xff, 0xef, 0xbe]), "/+++");
    }

    #[test]
    fn icon_path_refuses_to_climb_out_of_the_icon_directories() {
        assert_eq!(icon_path("../../../etc/passwd"), None);
        assert_eq!(icon_path(""), None);
        // An absolute path is allowed, but only if it is really there.
        assert_eq!(icon_path("/nonexistent/icon.png"), None);
    }
}
