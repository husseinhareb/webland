//! The environment every application Webland starts is given.
//!
//! Webland is a session of its own, not a window on the host's, and the thing
//! that decides which of the two an application actually joins is the D-Bus
//! session bus. A single-instance application, Thunar, Obsidian, anything
//! Electron, asks the bus whether a copy of itself is already running as this
//! user, and if one is, hands it the request and exits. Sharing the host's bus
//! therefore meant a launch from the browser opened a window on the host
//! desktop, and a launch on the host desktop opened one here, depending only on
//! which copy happened to start first.
//!
//! So the session runs a bus of its own and points every child at it. The bus
//! is started by whoever starts the session (the server) rather than here,
//! because the tray has to watch the same bus applications register their icons
//! on, and a bus this module kept to itself would leave those two looking in
//! different places.
//! It is also what gets the keyring and polkit prompters onto our display: the
//! bus activates them itself, and they inherit this environment like any other
//! child.
//!
//! `DISPLAY` gets the same treatment for the same reason. Inheriting the host's
//! meant that with no `XWayland` running, an X client started here connected to
//! the host's X server and its window was never seen again.
//!
//! What this cannot fix is an application that keeps its state in a directory
//! rather than on a bus: two Firefoxes cannot share one profile whatever the bus
//! says, so a second instance needs a profile of its own. That is what the
//! launch overrides in [`crate::apps`] are for.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdout, Command, Stdio};

/// The null sink every application Webland starts plays into.
///
/// Its own sink, not the machine's: the sound belongs to the session, and the
/// session is being watched in a browser that may be on another machine
/// entirely. The server captures this sink's monitor and streams it; the host's
/// own speakers keep playing the host's own audio and nothing else.
pub const AUDIO_SINK: &str = "webland";

/// What every child of the compositor is told about the session it joins.
#[derive(Debug)]
pub struct Env {
    /// Our Wayland socket.
    display: OsString,
    /// Our X display number, when `XWayland` is running.
    xdisplay: Option<u32>,
    /// The session bus children are pointed at, when there is one.
    bus: Option<String>,
}

/// A private `dbus-daemon`, alive for as long as the session.
#[derive(Debug)]
pub struct Bus {
    address: String,
    child: Child,
    /// Held open only so the daemon never writes into a closed pipe.
    _stdout: ChildStdout,
}

impl Env {
    /// Point children at our Wayland socket, our X display, and the session's
    /// own bus.
    ///
    /// The bus belongs to the caller rather than to this type: the tray has to
    /// watch the same one applications register on, so the session starts it
    /// once and hands it to both.
    #[must_use]
    pub fn new(display: &OsStr, xdisplay: Option<u32>, bus: Option<&Bus>) -> Self {
        Self {
            display: display.to_os_string(),
            xdisplay,
            bus: bus.map(|bus| bus.address.clone()),
        }
    }

    /// A command that will join this session when it is spawned.
    #[must_use]
    pub fn command(&self, program: &OsStr) -> Command {
        let mut command = Command::new(program);
        command.env("WAYLAND_DISPLAY", &self.display);
        // Toolkits pick their backend from this, and inherit `x11` verbatim when
        // webland itself was started from an X session, which sends everything
        // the long way round through `XWayland`.
        command.env("XDG_SESSION_TYPE", "wayland");
        // Into the session's own sink, which is what gets its audio to the
        // browser instead of to the speakers of whatever machine this is.
        command.env("PULSE_SINK", AUDIO_SINK);
        match self.xdisplay {
            Some(number) => command.env("DISPLAY", format!(":{number}")),
            None => command.env_remove("DISPLAY"),
        };
        match &self.bus {
            Some(address) => command.env("DBUS_SESSION_BUS_ADDRESS", address),
            // Better no bus than the host's: without one, an application starts
            // its own copy here instead of handing the window to the host's.
            None => command.env_remove("DBUS_SESSION_BUS_ADDRESS"),
        };
        command
    }
}

impl Bus {
    /// Where to reach this bus: what children are told, and what the tray
    /// watches.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Start `dbus-daemon` and read the address it prints.
    ///
    /// `--nofork` so the daemon is our child and dies with us; the address
    /// arrives on its stdout as a single line before anything else is written.
    #[must_use]
    pub fn start() -> Option<Self> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .spawn()
            .inspect_err(|err| tracing::warn!(%err, "could not start dbus-daemon"))
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let mut address = String::new();
        let mut reader = BufReader::new(&mut stdout);
        // Blocking, and deliberately: everything launched after this needs the
        // address, and the daemon prints it as soon as it is listening.
        if reader.read_line(&mut address).is_err() || address.trim().is_empty() {
            tracing::warn!("dbus-daemon printed no address");
            let _ = child.kill();
            return None;
        }
        Some(Self {
            address: address.trim().to_string(),
            child,
            _stdout: stdout,
        })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        // The session is over; a bus outliving it would be a stray daemon per
        // run of webland.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a child is told, as `Command` records it: `Some` sets, `None`
    /// removes.
    fn delta(env: &Env) -> Vec<(String, Option<String>)> {
        env.command(OsStr::new("true"))
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    /// Without a bus of our own, the host's must be taken away rather than
    /// left in place; inheriting it is the bug this module exists for. The
    /// same goes for `DISPLAY` with no `XWayland` running.
    #[test]
    fn nothing_of_the_host_session_is_inherited() {
        let env = Env {
            display: OsString::from("wayland-9"),
            xdisplay: None,
            bus: None,
        };
        let delta = delta(&env);
        assert!(delta.contains(&(String::from("DBUS_SESSION_BUS_ADDRESS"), None)));
        assert!(delta.contains(&(String::from("DISPLAY"), None)));
        assert!(delta.contains(&(
            String::from("WAYLAND_DISPLAY"),
            Some(String::from("wayland-9"))
        )));
    }

    /// With `XWayland` up, X clients get our display number and nothing else.
    #[test]
    fn xwayland_display_is_ours() {
        let env = Env {
            display: OsString::from("wayland-9"),
            xdisplay: Some(3),
            bus: None,
        };
        assert!(delta(&env).contains(&(String::from("DISPLAY"), Some(String::from(":3")))));
    }
}
