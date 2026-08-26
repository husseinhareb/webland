//! Wire format for the Webland protocol (backend side).
//!
//! The message set lives in `shared/protocol/` and is not defined yet.
//! Transport is deliberately left out: the first implementation will be
//! WebSocket, and the traits added here must not assume it, so that
//! WebTransport can be dropped in later.

/// Protocol version negotiated at connect time.
pub const VERSION: u32 = 0;
