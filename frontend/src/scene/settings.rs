//! The viewer's preferences.
//!
//! `localStorage`, unlike the window layout's `sessionStorage`: these belong to
//! the person at this browser, and should still be theirs tomorrow and in the
//! next tab. The keyboard layout is here for the same reason: it describes the
//! keyboard in front of the viewer, which is not necessarily the host's.

use std::str::FromStr;

use leptos::prelude::*;

/// Seconds a notification stays when its application leaves it to us.
pub const NOTIFICATION_SECONDS: u32 = 5;

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub clock_24h: RwSignal<bool>,
    pub show_stats: RwSignal<bool>,
    /// Application notifications are dropped rather than shown.
    pub do_not_disturb: RwSignal<bool>,
    pub notification_seconds: RwSignal<u32>,
    /// xkb names; an empty layout means the host's.
    pub layout: RwSignal<String>,
    pub variant: RwSignal<String>,
}

impl Settings {
    /// Read back what this browser chose, and keep writing it as it changes.
    #[must_use]
    pub fn load() -> Self {
        Self {
            clock_24h: persisted("webland.clock-24h", true),
            show_stats: persisted("webland.show-stats", true),
            do_not_disturb: persisted("webland.do-not-disturb", false),
            notification_seconds: persisted("webland.notification-seconds", NOTIFICATION_SECONDS),
            layout: persisted("webland.layout", String::new()),
            variant: persisted("webland.variant", String::new()),
        }
    }
}

/// A signal that starts from `localStorage` and is written back on change.
/// A stored value that no longer parses is ignored for the default.
fn persisted<T>(key: &'static str, default: T) -> RwSignal<T>
where
    T: FromStr + ToString + Send + Sync + 'static,
{
    let value = storage()
        .and_then(|storage| storage.get_item(key).ok().flatten())
        .and_then(|text| text.parse().ok())
        .unwrap_or(default);
    let signal = RwSignal::new(value);
    Effect::new(move |_| {
        let text = signal.with(ToString::to_string);
        if let Some(storage) = storage() {
            let _ = storage.set_item(key, &text);
        }
    });
    signal
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}
