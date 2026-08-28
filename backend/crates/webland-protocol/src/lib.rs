//! Wire format for the Webland protocol.
//!
//! The message set is specified in `shared/protocol/` and implemented once
//! here. Because the frontend is Rust, it depends on this crate directly, so
//! there is a single codec and the two ends cannot drift.
//!
//! Transport is deliberately left out: the first transport is WebSocket, and
//! nothing here may assume it, so WebTransport can be dropped in later. The
//! binary encoding is likewise undecided; these types fix the *shape* of the
//! three messages, not their bytes.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use webland_core::{Point, Rect, Size, SurfaceId};

/// Protocol version negotiated at connect time.
pub const VERSION: u32 = 0;

/// A framing error. The wire encoding is `bincode`; a transport delivers whole
/// messages, so a frame is one encoded [`ServerMessage`] or [`ClientMessage`].
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// A message could not be encoded.
    #[error("encode: {0}")]
    Encode(String),
    /// A frame could not be decoded into the expected message.
    #[error("decode: {0}")]
    Decode(String),
}

/// Encode a message into a single wire frame.
///
/// # Errors
/// Returns [`WireError::Encode`] if serialization fails.
pub fn encode<M: Serialize>(message: &M) -> Result<Vec<u8>, WireError> {
    bincode::serialize(message).map_err(|e| WireError::Encode(e.to_string()))
}

/// Decode a single wire frame into a message.
///
/// # Errors
/// Returns [`WireError::Decode`] if the bytes are not a valid `M`.
pub fn decode<M: DeserializeOwned>(bytes: &[u8]) -> Result<M, WireError> {
    bincode::deserialize(bytes).map_err(|e| WireError::Decode(e.to_string()))
}

/// Compress a raw pixel buffer for a [`Codec::Deflate`] frame. Level 1: fast,
/// and repetitive UI/terminal pixels still shrink enormously.
#[must_use]
pub fn deflate(bytes: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec(bytes, 1)
}

/// Decompress a [`Codec::Deflate`] payload back to raw pixels.
///
/// # Errors
/// Returns [`WireError::Decode`] if the data is not valid deflate.
pub fn inflate(bytes: &[u8]) -> Result<Vec<u8>, WireError> {
    miniz_oxide::inflate::decompress_to_vec(bytes)
        .map_err(|err| WireError::Decode(format!("inflate: {err:?}")))
}

/// How a surface frame's pixels are encoded in [`SurfaceFrame::payload`].
///
/// A terminal is just a highly compressible video: every surface travels this
/// path, `Raw` for the `wl_shm` easy case and a real codec for dmabuf clients
/// encoded on the GPU (see Decision 2 in the roadmap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    /// Uncompressed pixels. The `wl_shm` path, and the simplest to bring up.
    Raw,
    /// Raw pixels, deflate-compressed (see [`deflate`]/[`inflate`]). A CPU-side
    /// stopgap that makes the `wl_shm` path usable before GPU video encode.
    Deflate,
    /// H.264 bitstream, VA-API encoded directly from a client dmabuf.
    H264,
}

/// A surface appeared; the browser should allocate a scene node for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceCreated {
    pub id: SurfaceId,
    pub size: Size,
}

/// New contents for a surface.
///
/// `damage` bounds the changed region so an idle surface costs nothing;
/// `payload` is `codec`-encoded, its byte layout fixed by the encoding chosen
/// later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceFrame {
    pub id: SurfaceId,
    pub codec: Codec,
    pub damage: Vec<Rect>,
    pub payload: Vec<u8>,
}

/// Pressed or released, shared by pointer buttons and keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Press {
    Down,
    Up,
}

/// Input originating in the browser, on its way to a Wayland client.
///
/// The keyboard shape is intentionally minimal: xkb keymaps, key repeat and IME
/// are Phase 3 problems, not Phase 0 ones. `keycode` is a raw evdev code, the
/// unit the compositor ultimately needs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    PointerMotion { position: Point },
    PointerButton { button: u32, state: Press },
    PointerScroll { dx: f64, dy: f64 },
    Key { keycode: u32, state: Press },
}

/// Backend → browser. One of the two server-originated messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    SurfaceCreated(SurfaceCreated),
    SurfaceFrame(SurfaceFrame),
}

/// Browser → backend. Input plus the frame-pacing ack.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Input on its way to a Wayland client.
    Input(InputEvent),
    /// The browser has presented a frame and is ready for the next one. This is
    /// what lets the browser drive the frame clock (Decision 3): the server
    /// holds back until it arrives, so the in-flight queue stays bounded.
    FramePresented,
}

#[cfg(test)]
mod tests {
    use super::{
        ClientMessage, Codec, InputEvent, ServerMessage, SurfaceCreated, SurfaceFrame, decode,
        encode,
    };
    use webland_core::{Point, Rect, Size, SurfaceId};

    // Round-trips through the real wire codec (`encode`/`decode`) — the exact
    // bytes both the backend and the WASM frontend put on the wire.

    #[test]
    fn surface_created_round_trips() {
        let msg = ServerMessage::SurfaceCreated(SurfaceCreated {
            id: SurfaceId(1),
            size: Size {
                width: 800,
                height: 600,
            },
        });
        let frame = encode(&msg).unwrap();
        assert_eq!(msg, decode::<ServerMessage>(&frame).unwrap());
    }

    #[test]
    fn surface_frame_round_trips() {
        let msg = ServerMessage::SurfaceFrame(SurfaceFrame {
            id: SurfaceId(2),
            codec: Codec::H264,
            damage: vec![Rect {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            }],
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        });
        let frame = encode(&msg).unwrap();
        assert_eq!(msg, decode::<ServerMessage>(&frame).unwrap());
    }

    #[test]
    fn client_input_round_trips() {
        let msg = ClientMessage::Input(InputEvent::PointerMotion {
            position: Point { x: 12.0, y: 34.0 },
        });
        let frame = encode(&msg).unwrap();
        assert_eq!(msg, decode::<ClientMessage>(&frame).unwrap());
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode::<ClientMessage>(&[0xff, 0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn deflate_round_trips_and_shrinks() {
        use super::{deflate, inflate};
        let pixels = vec![0x20u8; 64 * 64 * 4]; // a solid surface, as UIs often are
        let packed = deflate(&pixels);
        assert!(packed.len() < pixels.len());
        assert_eq!(inflate(&packed).unwrap(), pixels);
    }
}
