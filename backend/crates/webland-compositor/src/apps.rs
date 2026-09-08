//! What the launcher can start.
//!
//! Read from freedesktop `.desktop` files, which is where every installed
//! application already describes itself — name, command, and whether it wants to
//! be shown in a menu at all. Nothing here is configured by hand.

use std::collections::HashMap;
use std::path::Path;

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
        let mut found: Vec<(String, String)> = Vec::new();
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
        found.sort_by_key(|(name, _)| name.to_lowercase());
        found.dedup_by(|a, b| a.0 == b.0);

        let mut names = Vec::with_capacity(found.len());
        let mut commands = HashMap::with_capacity(found.len());
        for (index, (name, exec)) in found.into_iter().enumerate() {
            let Ok(id) = u32::try_from(index) else { break };
            names.push(Application { id, name });
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
fn read_entry(path: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut exec = None;
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
            Some(("NoDisplay" | "Hidden" | "Terminal", "true")) => return None,
            Some(("Type", value)) if value.trim() != "Application" => return None,
            _ => {}
        }
    }
    let exec = strip_field_codes(&exec?);
    let name = name?;
    (!name.is_empty() && !exec.is_empty()).then_some((name, exec))
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
    use super::strip_field_codes;

    #[test]
    fn field_codes_are_dropped_but_arguments_are_not() {
        assert_eq!(strip_field_codes("firefox %u"), "firefox");
        assert_eq!(strip_field_codes("kitty -e fish %F"), "kitty -e fish");
        // A percent that is not a field code is just an argument.
        assert_eq!(strip_field_codes("app --pct 50%"), "app --pct 50%");
    }
}
