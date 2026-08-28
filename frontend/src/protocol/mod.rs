//! Webland protocol, browser side.
//!
//! Only the transport seam is declared here. The wire format itself is the
//! shared [`webland_protocol`] crate, compiled to WebAssembly and used verbatim
//! on both ends, so this side never reimplements the codec. WebSocket is the
//! first transport; nothing in this trait may assume it, so WebTransport can be
//! added without touching callers.
//!
//! Placeholder seam: nothing consumes these yet until Phase 2 (see docs/roadmap.md).
#![allow(dead_code, unused_imports)]

mod transport;
pub use transport::WebSocketTransport;

/// The wire vocabulary, re-exported straight from the shared crate. These are
/// the exact types the backend sends and receives — not a translated copy — so
/// the two ends cannot drift. `VERSION` is likewise the backend's constant.
pub use webland_protocol::{
    ClientMessage, Codec, InputEvent, Press, ServerMessage, SurfaceCreated, SurfaceFrame, VERSION,
    WireError, decode, encode,
};

/// Transport seam. Frames are opaque bytes; encoding lives in `webland-protocol`.
pub trait Transport {
    fn send(&self, frame: &[u8]);
    fn on_message(&self, handler: Box<dyn Fn(Vec<u8>)>);
    fn close(&self);
}
