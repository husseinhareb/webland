//! The Webland shell: window chrome, and the plumbing that connects the backend
//! transport to the scene.
//!
//! Windows are Leptos components over [`Scene`]'s signal, so dragging one or
//! raising it is a signal update and a restyle — no server round trip, no
//! re-encode. The canvas inside each window is the only part the compositor
//! knows about.
//!
//! [`Scene`]: crate::scene::Scene

mod capture;
mod chrome;
mod connect;
mod drag;
mod launcher;
mod menu;
mod overflow;
mod panel;
mod resize;
mod shell;
mod stats;
mod style;
mod systray;
mod tasks;
mod titlebar;
mod wallpaper;
mod window;

pub use shell::Desktop;
