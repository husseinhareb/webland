//! Webland protocol, browser side.
//!
//! Only the transport seam is declared here. The wire format itself is the
//! shared [`webland_protocol`] crate, compiled to WebAssembly and used verbatim
//! on both ends, so this side never reimplements the codec. WebSocket is the
//! first transport; nothing in this trait may assume it, so WebTransport can be
//! added without touching callers.

/// Negotiated at connect time. Re-exported so callers use one constant that is
/// guaranteed equal to the backend's `webland_protocol::VERSION`.
pub use webland_protocol::VERSION;

/// Transport seam. Frames are opaque bytes; encoding lives in `webland-protocol`.
pub trait Transport {
    fn send(&self, frame: &[u8]);
    fn on_message(&self, handler: Box<dyn Fn(Vec<u8>)>);
    fn close(&self);
}
