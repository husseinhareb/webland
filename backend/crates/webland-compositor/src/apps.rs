//! What the launcher can start.
//!
//! Read from freedesktop `.desktop` files, which is where every installed
//! application already describes itself — name, command, and whether it wants to
//! be shown in a menu at all. Nothing here is configured by hand, except the one
//! thing that cannot be read from anywhere: see [`parse_overrides`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use webland_protocol::Application;

/// The applications on this machine, and how to start each one.
#[derive(Debug, Default)]
pub struct Applications {
    /// Ordered for display; the index is the id the browser sends back.
    names: Vec<Application>,
    /// Already argv: quotes resolved, field codes dropped, `~/` expanded. The
    /// command is spawned directly, so there is no shell to do any of it later.
    commands: HashMap<u32, Vec<String>>,
    /// The `.desktop` files this listing was built from, so
    /// [`Applications::refresh`] can tell that one has come or gone.
    sources: Vec<PathBuf>,
}

impl Applications {
    /// Scan the usual directories.
    #[must_use]
    pub fn scan() -> Self {
        // Name, `Exec=` line, `Icon=` name, and the file's own basename — which
        // is the other name an entry goes by, and the one a person writing a
        // launch override is most likely to have typed.
        let mut found: Vec<(String, String, Option<String>, String)> = Vec::new();
        let home = home();
        let dirs = dirs(&home);
        let overrides = overrides(&home);
        if !overrides.is_empty() {
            tracing::info!(count = overrides.len(), "launch command overrides");
        }
        for dir in &dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "desktop")
                    && let Some((name, exec, icon)) = read_entry(&path)
                {
                    let stem = path
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    found.push((name, exec, icon, stem));
                }
            }
        }
        // Stable, and by name: the launcher is a list a person reads.
        found.sort_by_key(|(name, ..)| name.to_lowercase());
        found.dedup_by(|a, b| a.0 == b.0);

        let mut names = Vec::with_capacity(found.len());
        let mut commands = HashMap::with_capacity(found.len());
        for (index, (name, exec, icon, stem)) in found.into_iter().enumerate() {
            let Ok(id) = u32::try_from(index) else { break };
            let exec = override_for(&overrides, &name, &stem).map_or(exec, Clone::clone);
            names.push(Application {
                id,
                name,
                icon: icon.as_deref().and_then(icon_data_url),
            });
            commands.insert(id, argv(&exec, &home));
        }
        Self {
            names,
            commands,
            sources: sources(&dirs),
        }
    }

    /// Re-scan when an application has been installed or removed.
    ///
    /// Listing the directories is two `readdir` calls against names the kernel
    /// already has cached, which is cheap enough to do every pass — unlike the
    /// scan itself, which opens every file and base64s every icon. Returns
    /// whether the listing changed, and so wants announcing to the browser.
    ///
    /// Names rather than the directories' modification times: those are only as
    /// fine as a timer tick, so a removal in the same tick as the last look
    /// would leave a dead entry in the launcher for the rest of the session.
    ///
    /// ponytail: an edit to a file already there is still missed, since its name
    /// did not change. Installing and removing is what a launcher goes stale
    /// over; stat the files too if editing one in place ever matters.
    pub fn refresh(&mut self) -> bool {
        if sources(&dirs(&home())) == self.sources {
            return false;
        }
        let fresh = Self::scan();
        let changed = fresh.names != self.names;
        *self = fresh;
        changed
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
    /// `xdisplay` is the X server's number, when one is running: an X client
    /// reads `DISPLAY` and nothing else, and never learns there is a Wayland
    /// socket beside it.
    pub fn launch(&self, id: u32, display: &std::ffi::OsStr, xdisplay: Option<u32>) {
        let Some(argv) = self.commands.get(&id) else {
            tracing::warn!(id, "launch request for an unknown application");
            return;
        };
        let Some((program, arguments)) = argv.split_first() else {
            tracing::warn!(id, "launch request for an application with no command");
            return;
        };
        // Started from the user's home, not from wherever webland was launched:
        // a child inherits the compositor's working directory, so every file
        // dialog in every application would open in the source tree.
        let home = home();
        let mut launcher = std::process::Command::new(program);
        launcher.args(arguments).env("WAYLAND_DISPLAY", display);
        if let Some(number) = xdisplay {
            launcher.env("DISPLAY", format!(":{number}"));
        }
        if !home.is_empty() {
            launcher.current_dir(&home);
        }
        match launcher.spawn() {
            Ok(_) => tracing::info!(?argv, "launched"),
            Err(err) => tracing::warn!(?argv, %err, "could not launch"),
        }
    }
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// The directories holding `.desktop` files, the user's own first.
fn dirs(home: &str) -> [String; 2] {
    [
        format!("{home}/.local/share/applications"),
        String::from("/usr/share/applications"),
    ]
}

/// Every `.desktop` file in the given directories, in a stable order.
fn sources(dirs: &[String]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "desktop"))
        .collect();
    // `readdir` order is the filesystem's, and need not repeat between calls.
    paths.sort_unstable();
    paths
}

/// The user's launch overrides, read from `$XDG_CONFIG_HOME/webland/launch.conf`.
fn overrides(home: &str) -> HashMap<String, String> {
    let path = std::env::var("XDG_CONFIG_HOME").map_or_else(
        |_| format!("{home}/.config/webland/launch.conf"),
        |dir| format!("{dir}/webland/launch.conf"),
    );
    parse_overrides(&std::fs::read_to_string(path).unwrap_or_default())
}

/// The override for one application, if the user wrote one.
///
/// An exact match on either name first, then the longest override key either
/// name contains. The loose match is what makes the documented configuration
/// work at all: a person writes `Firefox = …`, and the entry that has to be
/// matched calls itself `Name=Firefox Web Browser` and lives in
/// `firefox.desktop`. Looking only for an exact `Name=` meant the whole feature
/// was quietly ignored on every distribution that ships that name.
///
/// Longest key wins so the answer does not depend on hash order: with both
/// `Firefox` and `Firefox Developer Edition` configured, the developer edition's
/// entry takes the one that actually names it.
fn override_for<'a>(
    overrides: &'a HashMap<String, String>,
    name: &str,
    stem: &str,
) -> Option<&'a String> {
    let name = name.to_lowercase();
    let stem = stem.to_lowercase();
    if let Some(exact) = overrides.get(&name).or_else(|| overrides.get(&stem)) {
        return Some(exact);
    }
    overrides
        .iter()
        .filter(|(key, _)| name.contains(key.as_str()) || stem.contains(key.as_str()))
        .max_by_key(|(key, _)| key.len())
        .map(|(_, command)| command)
}

/// Parse `Name = command` lines, keyed by lower-cased name.
///
/// This exists for one thing a `.desktop` file cannot express: a single-instance
/// application — Firefox, Chromium, anything Electron — hands its request to a
/// copy already running as the same user and exits, so the launcher's window
/// never appears at all. The second instance needs a profile of its own, and
/// only the person running webland knows where that should live:
///
/// ```text
/// Firefox  = firefox --no-remote --profile ~/.webland/firefox
/// Chromium = chromium --user-data-dir=~/.webland/chromium
/// ```
///
/// Create the profile directory first — Firefox will not make one whose parent
/// is missing, and says so in a dialog rather than on stderr.
///
/// The name written here need not be the entry's whole `Name=`: see
/// [`override_for`].
///
/// Blank lines and `#` comments are ignored. The first `=` separates, so a
/// command may contain more of them. The command is left as written; [`argv`]
/// is what turns it into something to spawn.
fn parse_overrides(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(name, command)| (name.trim().to_lowercase(), command.trim().to_string()))
        .filter(|(name, command)| !name.is_empty() && !command.is_empty())
        .collect()
}

/// Expand a leading `~/`, whether it opens a word or follows a flag's `=`.
///
/// The command is spawned directly, so there is no shell to do this and a
/// literal `~` would become a directory of that name.
fn expand_home(word: &str, home: &str) -> String {
    if let Some((flag, rest)) = word.split_once("=~/") {
        format!("{flag}={home}/{rest}")
    } else if let Some(rest) = word.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        word.to_string()
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
    let exec = exec?;
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

/// The `%f`, `%U`, … placeholders a `.desktop` Exec line may carry.
///
/// They stand for the files and URLs the entry was opened with; there are none
/// here, so they are dropped. Passing one through literally has the application
/// try to open a file called `%U` and give up, which is a launcher button that
/// does nothing.
const FIELD_CODES: [&str; 13] = [
    "%f", "%F", "%u", "%U", "%d", "%D", "%n", "%N", "%i", "%c", "%k", "%v", "%m",
];

/// One `Exec=` line or launch override, as the argv to spawn.
///
/// Splitting on whitespace is not enough and never was. A `.desktop` file quotes
/// any argument with a space in it — `--profile "~/My Profiles"`, `env
/// FOO="bar baz" app` — so whitespace splitting cuts those in half and hands the
/// quote characters themselves to the program. It also hides field codes: a
/// quoted `"%f"` is four characters, so the old length-2 test never saw it, and
/// inkscape and half of KDE launched with a filename of `%f`.
fn argv(command: &str, home: &str) -> Vec<String> {
    split_args(command)
        .into_iter()
        .filter(|word| !FIELD_CODES.contains(&word.as_str()))
        // `%%` is how an Exec line spells a literal percent.
        .map(|word| expand_home(&word.replace("%%", "%"), home))
        .collect()
}

/// Split a command line into arguments, honouring quotes and escapes.
///
/// Double quotes and backslash escapes are the Desktop Entry Specification's;
/// single quotes are not in it, but the launch override file is written by hand
/// and a person who types them means them.
fn split_args(command: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    // Distinct from `current` being non-empty, so `""` is an empty argument
    // rather than no argument at all.
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match quote {
            // Inside single quotes everything is itself, including backslashes.
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            Some(_) => match c {
                '"' => quote = None,
                // The spec escapes exactly these four inside double quotes;
                // anything else keeps its backslash, as a shell would.
                '\\' => match chars.next() {
                    Some(next @ ('"' | '\\' | '`' | '$')) => current.push(next),
                    Some(next) => {
                        current.push('\\');
                        current.push(next);
                    }
                    None => current.push('\\'),
                },
                _ => current.push(c),
            },
            None => match c {
                c if c.is_whitespace() => {
                    if started {
                        args.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                '"' | '\'' => {
                    quote = Some(c);
                    started = true;
                }
                '\\' => {
                    started = true;
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                _ => {
                    started = true;
                    current.push(c);
                }
            },
        }
    }
    if started {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::{argv, base64, icon_path, override_for, parse_overrides, sources, split_args};

    #[test]
    fn removing_an_entry_changes_what_a_directory_holds() {
        // What a refresh rests on: an application going away is visible from the
        // directory listing alone, without opening anything in it.
        let dir = std::env::temp_dir().join(format!("webland-apps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("gone.desktop");
        std::fs::write(&entry, "[Desktop Entry]\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not an application").unwrap();

        let dirs = [
            dir.to_string_lossy().into_owned(),
            format!("{}/absent", dir.display()),
        ];
        // A directory that is not there contributes nothing, rather than failing.
        let before = sources(&dirs);
        assert_eq!(before, vec![entry.clone()]);

        std::fs::remove_file(&entry).unwrap();
        assert!(sources(&dirs).is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn field_codes_are_dropped_but_arguments_are_not() {
        assert_eq!(argv("firefox %u", "/home/u"), ["firefox"]);
        assert_eq!(argv("kitty -e fish %F", "/home/u"), ["kitty", "-e", "fish"]);
        // A percent that is not a field code is just an argument.
        assert_eq!(argv("app --pct 50%", "/home/u"), ["app", "--pct", "50%"]);
        // A quoted field code is still a field code. This is the one that got
        // through: four characters, so a length-2 test never saw it, and the
        // application was handed a file named `%f` to open.
        assert_eq!(argv(r#"inkscape "%f""#, "/home/u"), ["inkscape"]);
        assert_eq!(argv("gimp %U %i %c", "/home/u"), ["gimp"]);
        // `%%` is a literal percent, and survives.
        assert_eq!(argv("app --fmt %%s", "/home/u"), ["app", "--fmt", "%s"]);
    }

    #[test]
    fn quoted_arguments_survive_as_one_argument() {
        assert_eq!(
            split_args(r#"env FOO="bar baz" app"#),
            ["env", "FOO=bar baz", "app"]
        );
        assert_eq!(
            split_args("app 'one two' three"),
            ["app", "one two", "three"]
        );
        // The spec's escapes, inside double quotes and out.
        assert_eq!(split_args(r#"app "a\"b" c\ d"#), ["app", r#"a"b"#, "c d"]);
        // A backslash the spec does not escape keeps itself, as a shell leaves
        // a Windows-shaped path alone.
        assert_eq!(split_args(r#"app "a\nb""#), ["app", r"a\nb"]);
        // An empty quoted argument is an argument.
        assert_eq!(split_args(r#"app "" x"#), ["app", "", "x"]);
        assert_eq!(split_args("   "), [] as [&str; 0]);
        // An unterminated quote takes what is there rather than losing it.
        assert_eq!(split_args(r#"app "half"#), ["app", "half"]);
    }

    #[test]
    fn home_is_expanded_inside_a_quoted_argument() {
        // The whole point of parsing quotes: a profile path with a space in it
        // used to arrive as two arguments, both carrying a quote character.
        assert_eq!(
            argv(r#"firefox --profile "~/My Profiles/w""#, "/home/u"),
            ["firefox", "--profile", "/home/u/My Profiles/w"]
        );
        assert_eq!(
            argv("chromium --user-data-dir=~/w", "/home/u"),
            ["chromium", "--user-data-dir=/home/u/w"]
        );
    }

    #[test]
    fn an_override_matches_the_name_a_person_would_have_typed() {
        let overrides = parse_overrides("Firefox = firefox --new-instance\n");
        // What every Linux distribution actually calls the entry, and the file
        // it lives in. Neither is the exact `Firefox` the README asks for, and
        // matching only the exact name is what made the whole feature a no-op.
        assert_eq!(
            override_for(&overrides, "Firefox Web Browser", "firefox").map(String::as_str),
            Some("firefox --new-instance")
        );
        assert_eq!(
            override_for(&overrides, "Web Browser", "firefox").map(String::as_str),
            Some("firefox --new-instance")
        );
        assert_eq!(
            override_for(&overrides, "Firefox", "org.mozilla.firefox").map(String::as_str),
            Some("firefox --new-instance")
        );
        // Something else entirely is still left alone.
        assert_eq!(override_for(&overrides, "GIMP", "gimp"), None);

        // The longest matching key wins, so two overrides that both match do
        // not depend on hash order to pick the one that names the entry.
        let both = parse_overrides("Firefox = firefox\nFirefox Developer Edition = firefox-dev\n");
        assert_eq!(
            override_for(
                &both,
                "Firefox Developer Edition",
                "firefox-developer-edition"
            )
            .map(String::as_str),
            Some("firefox-dev")
        );
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
    fn overrides_split_on_the_first_equals() {
        let parsed = parse_overrides(
            "# a note\n\n             Firefox = firefox --no-remote --profile ~/.webland/ff\n             Chromium = chromium --user-data-dir=~/w\n",
        );
        assert_eq!(
            parsed.get("firefox").map(String::as_str),
            Some("firefox --no-remote --profile ~/.webland/ff")
        );
        // Only the first `=` separates; the one in the flag is the command's.
        assert_eq!(
            parsed.get("chromium").map(String::as_str),
            Some("chromium --user-data-dir=~/w")
        );
        assert_eq!(parsed.len(), 2);
        // A `~` that is not a home directory is left alone.
        assert_eq!(
            argv("x ~backup file~", "/home/u"),
            ["x", "~backup", "file~"]
        );
    }

    #[test]
    fn icon_path_refuses_to_climb_out_of_the_icon_directories() {
        assert_eq!(icon_path("../../../etc/passwd"), None);
        assert_eq!(icon_path(""), None);
        // An absolute path is allowed, but only if it is really there.
        assert_eq!(icon_path("/nonexistent/icon.png"), None);
    }
}
