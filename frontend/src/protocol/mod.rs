//! Webland protocol, browser side.
//!
//! Only the transport seam lives here. The wire format is the shared
//! [`webland_protocol`] crate, compiled to WebAssembly and used verbatim on both
//! ends, so this side never reimplements the codec.

mod transport;

pub use transport::WebSocketTransport;
