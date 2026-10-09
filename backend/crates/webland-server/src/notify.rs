//! Desktop notifications: the `org.freedesktop.Notifications` server on the
//! session's bus.
//!
//! Like the tray, this is only possible because Webland runs a bus of its own:
//! the name is free there, and whoever calls `Notify` on it is one of our
//! applications. Each notification goes to every browser as a
//! [`Notification`]; a click or a close comes back and is answered on the bus
//! with `ActionInvoked` and `NotificationClosed`, which is how a chat client
//! learns to raise the conversation it was about.

// zbus rewrites the impl below into the bus-facing signatures it needs.
#![allow(clippy::unused_async, clippy::unused_async_trait_impl)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use webland_protocol::{Notification, ServerMessage};
use zbus::zvariant::OwnedValue;
use zbus::{Connection, ConnectionBuilder, interface};

use crate::transport::FrameSink;

const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";

/// `NotificationClosed` reasons, from the specification.
const DISMISSED: u32 = 2;
const CLOSED_BY_CALL: u32 = 3;

/// Ids of the notifications that asked for a `default` action, the one a
/// click on the notification itself invokes.
type Clickable = Arc<Mutex<HashSet<u32>>>;

struct Server {
    sink: FrameSink,
    next: AtomicU32,
    clickable: Clickable,
}

#[allow(clippy::unused_self)] // The macro's requirement.
#[interface(name = "org.freedesktop.Notifications")]
impl Server {
    /// No `body-markup`: bodies arrive as plain text and are drawn as text.
    fn get_capabilities(&self) -> Vec<String> {
        vec![String::from("body"), String::from("actions")]
    }

    #[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)] // The specification's signature.
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id == 0 {
            self.next.fetch_add(1, Ordering::Relaxed)
        } else {
            replaces_id
        };
        // `actions` alternates key and label; only the keys matter here.
        let default = actions.iter().step_by(2).any(|key| key == "default");
        {
            let mut clickable = self.clickable.lock().expect("notifications");
            if default {
                clickable.insert(id);
            } else {
                clickable.remove(&id);
            }
        }
        let critical = hints
            .get("urgency")
            .and_then(|urgency| u8::try_from(urgency).ok())
            == Some(2);
        let icon = app_icon.strip_prefix("file://").unwrap_or(&app_icon);
        self.sink.emit(ServerMessage::Notification(Notification {
            id,
            app: app_name,
            summary,
            body,
            icon: webland_compositor::apps::icon_data_url(icon),
            expire_timeout: timeout(expire_timeout, critical),
        }));
        id
    }

    async fn close_notification(&self, id: u32, #[zbus(connection)] connection: &Connection) {
        self.clickable.lock().expect("notifications").remove(&id);
        self.sink.emit(ServerMessage::NotificationClosed { id });
        closed(connection, id, CLOSED_BY_CALL).await;
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            String::from("Webland"),
            String::from("Webland"),
            String::from(env!("CARGO_PKG_VERSION")),
            String::from("1.2"),
        )
    }
}

/// The session's notification server.
#[derive(Debug)]
pub struct Notifications {
    connection: Connection,
    clickable: Clickable,
    sink: FrameSink,
}

impl Notifications {
    /// Take the notifications name on the session's bus. `None` when there is
    /// no bus, or the name is taken, which only means no notifications.
    pub async fn start(sink: FrameSink, bus: Option<&str>) -> Option<Arc<Self>> {
        let clickable = Clickable::default();
        let server = Server {
            sink: sink.clone(),
            // Zero means "no notification" in `replaces_id`.
            next: AtomicU32::new(1),
            clickable: clickable.clone(),
        };
        let connection = ConnectionBuilder::address(bus?)
            .and_then(|builder| builder.name(INTERFACE))
            .and_then(|builder| builder.serve_at(PATH, server))
            .ok()?
            .build()
            .await
            .inspect_err(|err| tracing::warn!(%err, "no notifications: server unavailable"))
            .ok()?;
        tracing::info!("notification server registered");
        Some(Arc::new(Self {
            connection,
            clickable,
            sink,
        }))
    }

    /// A browser clicked or closed a notification. Every browser drops it,
    /// and the application hears which of the two it was.
    pub async fn close(&self, id: u32, activated: bool) {
        let default = self.clickable.lock().expect("notifications").remove(&id);
        self.sink.emit(ServerMessage::NotificationClosed { id });
        if activated
            && default
            && let Err(err) = self
                .connection
                .emit_signal(
                    None::<()>,
                    PATH,
                    INTERFACE,
                    "ActionInvoked",
                    &(id, "default"),
                )
                .await
        {
            tracing::debug!(%err, id, "notification action not delivered");
        }
        closed(&self.connection, id, DISMISSED).await;
    }
}

async fn closed(connection: &Connection, id: u32, reason: u32) {
    if let Err(err) = connection
        .emit_signal(
            None::<()>,
            PATH,
            INTERFACE,
            "NotificationClosed",
            &(id, reason),
        )
        .await
    {
        tracing::debug!(%err, id, "notification close not delivered");
    }
}

/// A critical notification stays until somebody reads it, whatever its
/// application asked for.
fn timeout(expire_timeout: i32, critical: bool) -> i32 {
    if critical { 0 } else { expire_timeout }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use webland_protocol::ServerMessage;

    use super::{Notifications, timeout};
    use crate::transport::FrameSink;
    use crate::tray::tests::Bus;

    /// An application calls `Notify` exactly as libnotify does, and the
    /// browser is told; a replacement keeps its id. Skipped without
    /// `dbus-daemon`, like the tray's bus test.
    #[tokio::test]
    async fn a_notify_call_reaches_the_browser() {
        let Some(bus) = Bus::start() else {
            eprintln!("no dbus-daemon; skipping the notification bus test");
            return;
        };
        let sink = FrameSink::new();
        let mut browser = sink.subscribe();
        let _server = Notifications::start(sink, Some(&bus.address))
            .await
            .expect("server starts on its own bus");

        let app = zbus::ConnectionBuilder::address(bus.address.as_str())
            .expect("bus address")
            .build()
            .await
            .expect("app connects");
        let proxy = zbus::Proxy::new(
            &app,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .await
        .expect("server is on the bus");
        let mut id = 0;
        for replaces in [0, 1] {
            let args = (
                "Chat",
                replaces * id,
                "",
                "New message",
                "hello",
                vec!["default", "Open"],
                HashMap::<&str, zbus::zvariant::Value<'_>>::new(),
                -1i32,
            );
            let answered: u32 = proxy
                .call("Notify", &args)
                .await
                .expect("notify is answered");
            // The second call replaces the first, so keeps its id.
            if replaces == 1 {
                assert_eq!(answered, id);
            }
            id = answered;
        }

        for _ in 0..2 {
            match browser.recv().await.expect("the browser hears it") {
                ServerMessage::Notification(n) => {
                    assert_eq!(
                        (n.id, n.summary.as_str(), n.body.as_str()),
                        (id, "New message", "hello")
                    );
                    assert_eq!(n.expire_timeout, -1);
                }
                other => panic!("expected a notification, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_critical_notification_stays() {
        assert_eq!(timeout(-1, false), -1);
        assert_eq!(timeout(1200, false), 1200);
        assert_eq!(timeout(1200, true), 0);
    }
}
