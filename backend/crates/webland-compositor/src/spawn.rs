//! The environment every application Webland starts is given.
//!
//! Webland is a session of its own, not a window on the host's, and the thing
//! that decides which of the two an application actually joins is the D-Bus
//! session bus. A single-instance application — Thunar, Obsidian, anything
//! Electron — asks the bus whether a copy of itself is already running as this
//! user, and if one is, hands it the request and exits. Sharing the host's bus
//! therefore meant a launch from the browser opened a window on the host
//! desktop, and a launch on the host desktop opened one here, depending only on
//! which copy happened to start first.
//!
//! So this starts a session bus of Webland's own and points every child at it.
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

/// What every child of the compositor is told about the session it joins.
#[derive(Debug)]
pub struct Env {
    /// Our Wayland socket.
    display: OsString,
    /// Our X display number, when `XWayland` is running.
    xdisplay: Option<u32>,
    /// Our own session bus, when one could be started.
    bus: Option<Bus>,
}

/// A private `dbus-daemon`, alive for as long as this session.
#[derive(Debug)]
struct Bus {
    address: String,
    child: Child,
    /// Held open only so the daemon never writes into a closed pipe.
    _stdout: ChildStdout,
}

impl Env {
    /// Point children at our Wayland socket, our X display, and a session bus
    /// of our own.
    ///
    /// A bus that cannot be started is not fatal — `dbus-daemon` need not be
    /// installed — but it is worth saying out loud, because the symptom is
    /// windows opening on the wrong desktop rather than anything failing.
    #[must_use]
    pub fn new(display: &OsStr, xdisplay: Option<u32>) -> Self {
        let bus = Bus::start();
        if let Some(bus) = &bus {
            tracing::info!(address = %bus.address, "session bus");
        } else {
            tracing::warn!(
                "no session bus of our own: single-instance applications may open on the host desktop"
            );
        }
        Self {
            display: display.to_os_string(),
            xdisplay,
            bus,
        }
    }

    /// A command that will join this session when it is spawned.
    #[must_use]
    pub fn command(&self, program: &OsStr) -> Command {
        let mut command = Command::new(program);
        command.env("WAYLAND_DISPLAY", &self.display);
        // Toolkits pick their backend from this, and inherit `x11` verbatim when
        // webland itself was started from an X session — which sends everything
        // the long way round through `XWayland`.
        command.env("XDG_SESSION_TYPE", "wayland");
        match self.xdisplay {
            Some(number) => command.env("DISPLAY", format!(":{number}")),
            None => command.env_remove("DISPLAY"),
        };
        match &self.bus {
            Some(bus) => command.env("DBUS_SESSION_BUS_ADDRESS", &bus.address),
            // Better no bus than the host's: without one, an application starts
            // its own copy here instead of handing the window to the host's.
            None => command.env_remove("DBUS_SESSION_BUS_ADDRESS"),
        };
        command
    }
}

impl Bus {
    /// Start `dbus-daemon` and read the address it prints.
    ///
    /// `--nofork` so the daemon is our child and dies with us; the address
    /// arrives on its stdout as a single line before anything else is written.
    fn start() -> Option<Self> {
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
    /// left in place — inheriting it is the bug this module exists for. The
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
