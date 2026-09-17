//! H.264 decode via `WebCodecs`.
//!
//! The compositor encodes surfaces on the GPU (Decision 2) and this is the other
//! end of that: an Annex B access unit in, a `VideoFrame` out, which the renderer
//! hands straight to the GPU without the pixels ever being touched by JavaScript
//! or WASM.

mod annexb;
mod decoder;

pub use decoder::Decoder;
