//! Browser input capture.
//!
//! Pointer events, wheel events and key events are translated to
//! `webland-protocol` [`InputEvent`]s and sent to the backend, which injects them
//! into the Wayland seat. Keyboard mapping is `KeyboardEvent.code` → Linux evdev
//! keycode; it covers a common subset, not (yet) IME or every key.
//!
//! [`InputEvent`]: webland_protocol::InputEvent

mod clipboard;
mod dom;
mod keyboard;
mod keymap;
mod locked;
mod pointer;
mod shortcuts;
mod wheel;
mod wire;

pub use clipboard::set_clipboard;
pub use keyboard::{lock_keyboard, release_modifiers, unlock_keyboard};
pub use locked::viewport;
pub use shortcuts::present_and_focus;
pub use wire::wire;
