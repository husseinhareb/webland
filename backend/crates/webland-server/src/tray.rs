//! The system tray: a freedesktop `StatusNotifier` host on the session's bus.
//!
//! Tray icons are not windows and never were. An application does not draw
//! one, it publishes an `org.kde.StatusNotifierItem` on the session bus and
//! waits for a panel to come and ask about it. Nothing appears at all until
//! something registers itself as the *watcher* that items look for, and as a
//! *host* that says somebody is willing to show them. This module is both, and
//! what it learns goes to the browser as [`TrayItem`]s for the panel to draw.
//!
//! Webland starts its own session bus (see the compositor's `spawn` module),
//! which makes this simpler than a tray on a shared bus: there is never another
//! watcher to lose the race to, and every item on this bus belongs to this
//! session. Applications started outside Webland keep their icons out there.
//!
//! What crosses the wire is a picture and a label, never a D-Bus type: the
//! item's own ARGB pixmap re-encoded as a PNG, or the file its icon name
//! resolves to in the icon theme. A click comes back the other way and is
//! turned into `Activate`, `SecondaryActivate` or a `com.canonical.dbusmenu`
//! event on the item itself.

// zbus rewrites the impl below into the bus-facing signatures it needs, which
// is why these methods are `async` and take `&self` whether or not the body
// does anything with either. The attribute has to live here: an allow on the
// impl block does not survive the macro.
#![allow(clippy::unused_async, clippy::unused_async_trait_impl)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use webland_protocol::{ServerMessage, TrayItem, TrayMenuItem};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{Connection, ConnectionBuilder, interface, proxy};

use crate::transport::FrameSink;

/// One dbusmenu node as it comes off the wire: `(id, properties, children)`.
type MenuLayout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

/// The properties worth asking a menu for. Asking for everything means every
/// icon and shortcut of every row, none of which is drawn.
const MENU_PROPERTIES: [&str; 7] = [
    "label",
    "enabled",
    "visible",
    "type",
    "toggle-type",
    "toggle-state",
    "children-display",
];

/// The biggest pixmap worth sending. Items commonly offer 16, 22, 24 and 32;
/// anything above this is a scaled-up version of one of them and costs the
/// wire more than it gives the eye.
const MAX_PIXMAP: i32 = 64;

#[proxy(
    interface = "org.kde.StatusNotifierItem",
    default_path = "/StatusNotifierItem"
)]
trait StatusNotifierItem {
    #[zbus(property)]
    fn title(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn icon_name(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn icon_theme_path(&self) -> zbus::Result<String>;
    /// `(width, height, ARGB bytes)`, in whatever sizes the item offers.
    #[zbus(property)]
    fn icon_pixmap(&self) -> zbus::Result<Vec<(i32, i32, Vec<u8>)>>;
    #[zbus(property)]
    fn menu(&self) -> zbus::Result<OwnedObjectPath>;

    fn activate(&self, x: i32, y: i32) -> zbus::Result<()>;
    fn secondary_activate(&self, x: i32, y: i32) -> zbus::Result<()>;
}

#[proxy(interface = "com.canonical.dbusmenu")]
trait DbusMenu {
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<&str>,
    ) -> zbus::Result<(u32, MenuLayout)>;
    fn event(&self, id: i32, event_id: &str, data: OwnedValue, timestamp: u32) -> zbus::Result<()>;
    fn about_to_show(&self, id: i32) -> zbus::Result<bool>;
}

/// The addresses of every item currently registered, as `bus-name/object-path`.
type Items = Arc<Mutex<Vec<String>>>;

/// The watcher object served on the bus. Items call it to announce themselves.
struct Watcher {
    items: Items,
    /// Told whenever the list changes, so the browser hears about it.
    changed: tokio::sync::mpsc::UnboundedSender<()>,
}

#[allow(clippy::unused_self)] // Likewise the macro's requirement.
#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// An item announcing itself.
    ///
    /// `service` is either a bus name or, from applications that follow the
    /// other half of the specification, just an object path, in which case
    /// the sender of the message is the bus name. Both spellings end up as one
    /// address here so the rest of the module has one thing to handle.
    async fn register_status_notifier_item(
        &mut self,
        service: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        let sender = header
            .sender()
            .map_or_else(|| service.to_string(), ToString::to_string);
        let address = if service.starts_with('/') {
            format!("{sender}{service}")
        } else {
            service.to_string()
        };
        let mut items = self.items.lock().expect("tray items");
        if !items.contains(&address) {
            tracing::info!(item = %address, "tray item registered");
            items.push(address);
            let _ = self.changed.send(());
        }
    }

    /// A host announcing itself. We are the host, and there is no second panel
    /// on this bus, but an item may well ask.
    async fn register_status_notifier_host(&mut self, service: &str) {
        tracing::debug!(host = %service, "another tray host on this bus");
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().expect("tray items").clone()
    }

    /// The property items check before bothering to publish anything.
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

/// The session's tray.
#[derive(Debug)]
pub struct Tray {
    connection: Connection,
    items: Items,
}

impl Tray {
    /// Bring up the watcher on the session's own bus and start following it.
    ///
    /// `bus` is the bus the compositor points applications at. Watching any
    /// other one is watching the wrong place: tray icons are registered by
    /// those applications, on that bus, while the host's belongs to the host's
    /// own desktop, which already has a panel holding the watcher name.
    ///
    /// `None` when the session has no bus of its own, which is not fatal: it
    /// only means no tray.
    pub async fn start(sink: FrameSink, bus: Option<&str>) -> Option<Arc<Self>> {
        Self::serve(ConnectionBuilder::address(bus?).ok()?, sink).await
    }

    /// The same, on a bus the caller chose. Separate from [`Self::start`] only
    /// so a test can run a whole tray on a bus of its own.
    async fn serve(builder: ConnectionBuilder<'_>, sink: FrameSink) -> Option<Arc<Self>> {
        let items: Items = Arc::new(Mutex::new(Vec::new()));
        let (nudge, mut nudges) = tokio::sync::mpsc::unbounded_channel();
        let watcher = Watcher {
            items: items.clone(),
            changed: nudge.clone(),
        };
        let connection = builder
            .name("org.kde.StatusNotifierWatcher")
            .and_then(|builder| builder.serve_at("/StatusNotifierWatcher", watcher))
            .ok()?
            .build()
            .await
            .inspect_err(|err| tracing::warn!(%err, "no system tray: watcher unavailable"))
            .ok()?;

        // Also take the host name. Some applications look for a host on the bus
        // rather than asking the watcher, and publish nothing until they see one.
        if let Ok(bus) = zbus::fdo::DBusProxy::new(&connection).await {
            let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
            if let Ok(name) = host.as_str().try_into() {
                let _ = bus
                    .request_name(name, zbus::fdo::RequestNameFlags::DoNotQueue.into())
                    .await;
            }
        }
        tracing::info!("system tray: watcher and host registered");

        let tray = Arc::new(Self {
            connection: connection.clone(),
            items: items.clone(),
        });

        // An item that goes away takes its bus name with it, and says nothing:
        // a crashed application would otherwise leave its icon in the panel for
        // the rest of the session.
        tokio::spawn(forget_departed(connection.clone(), items, nudge.clone()));
        // An item that changes its icon or title says so on its own interface.
        // One stream for all of them, since a change to any means the same
        // thing here: ask everybody again.
        tokio::spawn(follow_item_changes(connection, nudge));

        // Publishing is one task so the bus handlers never wait on D-Bus calls
        // of their own: every reason to republish arrives as a nudge here.
        let publisher = tray.clone();
        tokio::spawn(async move {
            while nudges.recv().await.is_some() {
                sink.emit(ServerMessage::Tray {
                    items: publisher.snapshot().await,
                });
            }
        });

        Some(tray)
    }

    /// Every item, as the browser should draw it.
    ///
    /// Asked fresh rather than cached: this is a handful of D-Bus round trips
    /// on a bus with a handful of clients, and it happens when a browser
    /// connects or an item changes, not per frame.
    pub async fn snapshot(&self) -> Vec<TrayItem> {
        let addresses = self.items.lock().expect("tray items").clone();
        let mut items = Vec::with_capacity(addresses.len());
        for address in addresses {
            let Some(item) = self.proxy(&address).await else {
                continue;
            };
            items.push(TrayItem {
                id: address,
                title: item.title().await.unwrap_or_default(),
                icon: icon_for(&item).await,
            });
        }
        items
    }

    /// Click an item: its own action, or its alternate one.
    pub async fn activate(&self, id: &str, secondary: bool) {
        let Some(item) = self.proxy(id).await else {
            return;
        };
        // Coordinates are where the click happened on screen, which an item may
        // use to place a window near the pointer. The pointer is in a browser
        // on another machine as far as this bus is concerned, so there is no
        // honest answer and the origin is the conventional one.
        let clicked = if secondary {
            item.secondary_activate(0, 0).await
        } else {
            item.activate(0, 0).await
        };
        if let Err(err) = clicked {
            tracing::debug!(%err, item = %id, "tray item refused the click");
        }
    }

    /// One item's menu, fetched now.
    pub async fn menu(&self, id: &str) -> Vec<TrayMenuItem> {
        let Some(menu) = self.menu_proxy(id).await else {
            return Vec::new();
        };
        // Applications build their menu in answer to this, so it comes before
        // the layout is read rather than after.
        let _ = menu.about_to_show(0).await;
        // Depth -1 is the whole tree: submenus arrive with it, and the browser
        // can open them without another round trip.
        let Ok((_, (_, _, children))) = menu.get_layout(0, -1, MENU_PROPERTIES.to_vec()).await
        else {
            return Vec::new();
        };
        children.into_iter().filter_map(menu_item).collect()
    }

    /// Pick a row of an item's menu.
    pub async fn click(&self, id: &str, item: i32) {
        let Some(menu) = self.menu_proxy(id).await else {
            return;
        };
        let _ = menu.about_to_show(item).await;
        if let Err(err) = menu.event(item, "clicked", OwnedValue::from(0i32), 0).await {
            tracing::debug!(%err, %item, "tray menu item refused the click");
        }
    }

    async fn proxy(&self, address: &str) -> Option<StatusNotifierItemProxy<'_>> {
        let (bus, path) = split(address);
        StatusNotifierItemProxy::builder(&self.connection)
            .destination(bus)
            .ok()?
            .path(path)
            .ok()?
            .build()
            .await
            .ok()
    }

    async fn menu_proxy(&self, address: &str) -> Option<DbusMenuProxy<'_>> {
        let item = self.proxy(address).await?;
        let path = item.menu().await.ok()?;
        let (bus, _) = split(address);
        DbusMenuProxy::builder(&self.connection)
            .destination(bus)
            .ok()?
            .path(path)
            .ok()?
            .build()
            .await
            .ok()
    }
}

/// Split an item's address into the bus name and the object path.
///
/// The path is optional in what items send; `/StatusNotifierItem` is what the
/// specification says to assume, and what applications that omit it serve.
fn split(address: &str) -> (String, String) {
    address.find('/').map_or_else(
        || (address.to_owned(), String::from("/StatusNotifierItem")),
        |at| (address[..at].to_owned(), address[at..].to_owned()),
    )
}

/// Drop items whose bus name has gone.
async fn forget_departed(
    connection: Connection,
    items: Items,
    changed: tokio::sync::mpsc::UnboundedSender<()>,
) {
    let Ok(bus) = zbus::fdo::DBusProxy::new(&connection).await else {
        return;
    };
    let Ok(mut owners) = bus.receive_name_owner_changed().await else {
        return;
    };
    while let Some(signal) = owners.next().await {
        let Ok(args) = signal.args() else { continue };
        // A new owner means a rename, not a departure; only an empty one is a
        // client that has gone.
        if args.new_owner().is_some() {
            continue;
        }
        let name = args.name().to_string();
        let mut held = items.lock().expect("tray items");
        let before = held.len();
        held.retain(|address| split(address).0 != name);
        if held.len() != before {
            tracing::info!(item = %name, "tray item gone");
            drop(held);
            let _ = changed.send(());
        }
    }
}

/// Re-ask about everything whenever any item says something about itself
/// changed, a new icon, a new title, a new status.
async fn follow_item_changes(
    connection: Connection,
    changed: tokio::sync::mpsc::UnboundedSender<()>,
) {
    let Ok(rule) = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.kde.StatusNotifierItem")
        .map(zbus::MatchRuleBuilder::build)
    else {
        return;
    };
    let Ok(stream) = zbus::MessageStream::for_match_rule(rule, &connection, None).await else {
        return;
    };
    let mut stream = Box::pin(stream);
    while let Some(message) = stream.next().await {
        if message.is_ok() {
            let _ = changed.send(());
        }
    }
}

/// An item's icon, as a `data:` URL the browser can put in an `<img>`.
///
/// The item's own pixmap first, because it is what the application drew and is
/// current; a network applet's icon says whether it is connected. The icon
/// name is the fallback, resolved against the icon theme like any other.
async fn icon_for(item: &StatusNotifierItemProxy<'_>) -> Option<String> {
    if let Ok(pixmaps) = item.icon_pixmap().await
        && let Some((width, height, argb)) = best_pixmap(&pixmaps)
        && let Some(png) = png(width, height, &argb)
    {
        return Some(format!(
            "data:image/png;base64,{}",
            webland_compositor::apps::base64(&png)
        ));
    }
    let name = item.icon_name().await.ok()?;
    if name.is_empty() {
        return None;
    }
    webland_compositor::apps::icon_data_url(&name)
}

/// The largest pixmap that is not bigger than [`MAX_PIXMAP`], or the smallest
/// one offered if every one of them is.
fn best_pixmap(pixmaps: &[(i32, i32, Vec<u8>)]) -> Option<(i32, i32, Vec<u8>)> {
    let usable = |(width, height, data): &&(i32, i32, Vec<u8>)| {
        let pixels = usize::try_from(*width)
            .ok()
            .zip(usize::try_from(*height).ok())
            .map(|(width, height)| width * height * 4);
        pixels.is_some_and(|pixels| pixels > 0 && data.len() >= pixels)
    };
    pixmaps
        .iter()
        .filter(usable)
        .filter(|(width, _, _)| *width <= MAX_PIXMAP)
        .max_by_key(|(width, _, _)| *width)
        .or_else(|| {
            pixmaps
                .iter()
                .filter(usable)
                .min_by_key(|(width, _, _)| *width)
        })
        .cloned()
}

/// Encode an SNI pixmap as a PNG.
///
/// SNI pixmaps are 32-bit ARGB in network byte order: the bytes are `A R G B`
/// per pixel, with straight alpha. PNG wants `R G B A`, which is the same four
/// values rotated by one, and no other conversion: the alpha is already
/// straight, so nothing has to be un-premultiplied.
fn png(width: i32, height: i32, argb: &[u8]) -> Option<Vec<u8>> {
    let (width, height) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    let pixels = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if argb.len() < pixels {
        return None;
    }
    let mut rgba = Vec::with_capacity(pixels);
    for [a, r, g, b] in argb[..pixels].as_chunks::<4>().0 {
        rgba.extend_from_slice(&[*r, *g, *b, *a]);
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// Turn one dbusmenu node into a row, or drop it.
///
/// Dropped: rows the application marked invisible, and rows with no label that
/// are not separators; a menu is read, and a blank row cannot be.
#[allow(clippy::needless_pass_by_value)] // `try_into` consumes it.
fn menu_item(value: OwnedValue) -> Option<TrayMenuItem> {
    let (id, properties, children): MenuLayout = value.try_into().ok()?;
    if !property_bool(&properties, "visible").unwrap_or(true) {
        return None;
    }
    if property_string(&properties, "type").as_deref() == Some("separator") {
        return Some(TrayMenuItem {
            id,
            label: String::new(),
            enabled: false,
            checked: None,
            separator: true,
            children: Vec::new(),
        });
    }
    let label = clean(property_string(&properties, "label")?);
    if label.is_empty() {
        return None;
    }
    let checked = match property_string(&properties, "toggle-type").as_deref() {
        Some("checkmark" | "radio") => {
            Some(property_int(&properties, "toggle-state").unwrap_or(0) > 0)
        }
        _ => None,
    };
    Some(TrayMenuItem {
        id,
        label,
        enabled: property_bool(&properties, "enabled").unwrap_or(true),
        checked,
        separator: false,
        children: children.into_iter().filter_map(menu_item).collect(),
    })
}

/// A dbusmenu label as a person should read it: `_` marks the keyboard
/// mnemonic and is not part of the word, except `__`, which is one underscore.
#[allow(clippy::needless_pass_by_value)] // Called on values the parser owns.
fn clean(label: String) -> String {
    let mut out = String::with_capacity(label.len());
    let mut characters = label.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '_' if characters.peek() == Some(&'_') => {
                out.push('_');
                characters.next();
            }
            '_' => {}
            '\t' => out.push(' '),
            other => out.push(other),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn property_string(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = properties.get(key)?.try_clone().ok()?;
    String::try_from(value).ok()
}

fn property_bool(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    bool::try_from(properties.get(key)?).ok()
}

fn property_int(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    i32::try_from(properties.get(key)?).ok()
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    use super::{Tray, clean, png, split};
    use crate::transport::FrameSink;

    /// A pixmap's bytes are `A R G B`; a PNG's are `R G B A`. Getting this
    /// wrong is not a crash, it is every tray icon coming out the wrong colour,
    /// which is why it is worth a test of its own.
    #[test]
    fn a_pixmap_becomes_a_png_with_the_channels_in_the_right_order() {
        // One opaque red pixel, in SNI's order.
        let encoded = png(1, 1, &[0xff, 0xff, 0x00, 0x00]).expect("one pixel encodes");
        let decoder = ::png::Decoder::new(encoded.as_slice());
        let mut reader = decoder.read_info().expect("valid png");
        let mut pixels = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut pixels).expect("one frame");
        assert_eq!((info.width, info.height), (1, 1));
        assert_eq!(&pixels[..4], &[0xff, 0x00, 0x00, 0xff]);
    }

    /// Short data is a malformed item, not a reason to read past the end.
    #[test]
    fn a_pixmap_that_is_too_small_is_refused() {
        assert!(png(4, 4, &[0; 16]).is_none());
    }

    #[test]
    fn an_item_address_splits_into_a_bus_name_and_a_path() {
        assert_eq!(
            split(":1.42/org/ayatana/NotificationItem/item"),
            (
                String::from(":1.42"),
                String::from("/org/ayatana/NotificationItem/item")
            )
        );
        // No path: the one the specification says to assume.
        assert_eq!(
            split(":1.7"),
            (String::from(":1.7"), String::from("/StatusNotifierItem"))
        );
    }

    #[test]
    fn a_mnemonic_is_not_part_of_the_label() {
        assert_eq!(clean(String::from("_Quit")), "Quit");
        assert_eq!(clean(String::from("Save __As")), "Save _As");
    }

    /// The whole watcher, on a bus of its own: an item registers itself exactly
    /// as a real application does, and must come back out of `snapshot` with
    /// the picture it published.
    ///
    /// Skipped when `dbus-daemon` is not installed; this is the one test here
    /// that needs a machine, and a missing bus is not a failing tray.
    #[tokio::test]
    async fn an_item_that_registers_is_seen_with_its_icon() {
        let Some(bus) = Bus::start() else {
            eprintln!("no dbus-daemon; skipping the tray's bus test");
            return;
        };

        let tray = Tray::serve(
            zbus::ConnectionBuilder::address(bus.address.as_str()).expect("bus address"),
            FrameSink::new(),
        )
        .await
        .expect("watcher starts on its own bus");

        // An application publishing one red pixel of an icon.
        let item = zbus::ConnectionBuilder::address(bus.address.as_str())
            .expect("bus address")
            .name("org.kde.StatusNotifierItem-1-1")
            .expect("item name")
            .serve_at("/StatusNotifierItem", Item)
            .expect("serve item")
            .build()
            .await
            .expect("item connects");

        let watcher = zbus::Proxy::new(
            &item,
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
        )
        .await
        .expect("watcher is on the bus");
        watcher
            .call_method("RegisterStatusNotifierItem", &("/StatusNotifierItem",))
            .await
            .expect("registration is accepted");

        let items = tray.snapshot().await;
        assert_eq!(items.len(), 1, "the registered item is in the tray");
        assert_eq!(items[0].title, "Test Item");
        let icon = items[0].icon.as_deref().expect("the item's own pixmap");
        assert!(icon.starts_with("data:image/png;base64,"), "{icon}");
    }

    /// The item an application would publish.
    struct Item;

    #[allow(clippy::unused_self)] // zbus's signatures again.
    #[zbus::interface(name = "org.kde.StatusNotifierItem")]
    impl Item {
        #[zbus(property)]
        fn title(&self) -> String {
            String::from("Test Item")
        }
        #[zbus(property)]
        fn icon_name(&self) -> String {
            String::new()
        }
        #[zbus(property)]
        fn icon_theme_path(&self) -> String {
            String::new()
        }
        #[zbus(property)]
        fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
            vec![(1, 1, vec![0xff, 0xff, 0x00, 0x00])]
        }
        #[zbus(property)]
        fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
            zbus::zvariant::OwnedObjectPath::try_from("/MenuBar").expect("static path")
        }
    }

    /// A private `dbus-daemon`, so the test never touches the machine's own
    /// session bus; registering a watcher there would take tray icons away
    /// from whatever panel the user is actually running.
    struct Bus {
        address: String,
        child: std::process::Child,
    }

    impl Bus {
        fn start() -> Option<Self> {
            let mut child = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address"])
                .stdout(Stdio::piped())
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            let mut address = String::new();
            if BufReader::new(stdout).read_line(&mut address).is_err() || address.trim().is_empty()
            {
                let _ = child.kill();
                return None;
            }
            Some(Self {
                address: address.trim().to_string(),
                child,
            })
        }
    }

    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
