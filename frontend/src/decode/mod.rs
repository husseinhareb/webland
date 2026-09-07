//! H.264 decode via WebCodecs.
//!
//! The compositor encodes surfaces on the GPU (Decision 2) and this is the
//! other end of that: an Annex B access unit in, a `VideoFrame` out, which the
//! renderer hands straight to the GPU without the pixels ever being touched by
//! JavaScript or WASM.

use std::cell::Cell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{
    EncodedVideoChunk, EncodedVideoChunkInit, EncodedVideoChunkType, VideoDecoderConfig,
    VideoDecoderInit, VideoFrame,
};

/// A WebCodecs decoder, configured from the first keyframe it is given.
pub struct Decoder {
    inner: web_sys::VideoDecoder,
    configured: Rc<Cell<bool>>,
    timestamp: Cell<i32>,
    // Kept alive for as long as the decoder is: dropping a Closure detaches the
    // JS callback, and the decoder would then decode into nothing.
    _on_frame: Closure<dyn FnMut(JsValue)>,
    _on_error: Closure<dyn FnMut(JsValue)>,
}

impl Decoder {
    /// Build a decoder that hands each decoded frame to `on_frame`.
    ///
    /// # Errors
    /// Returns the JS error if the browser has no `VideoDecoder` — which is the
    /// signal to stay on the pixel codecs.
    pub fn new(mut on_frame: impl FnMut(&VideoFrame) + 'static) -> Result<Self, JsValue> {
        let configured = Rc::new(Cell::new(false));
        let on_frame = Closure::wrap(Box::new(move |value: JsValue| {
            if let Ok(frame) = value.dyn_into::<VideoFrame>() {
                on_frame(&frame);
                // VideoFrames hold a GPU allocation and the decoder pool is
                // small; not closing one stalls decoding within a few frames.
                frame.close();
            }
        }) as Box<dyn FnMut(JsValue)>);
        let failed = configured.clone();
        let on_error = Closure::wrap(Box::new(move |value: JsValue| {
            web_sys::console::error_2(&JsValue::from_str("video decode failed"), &value);
            // Give up on the current configuration; the next keyframe rebuilds
            // it, which is also how a resize recovers.
            failed.set(false);
        }) as Box<dyn FnMut(JsValue)>);

        let init = VideoDecoderInit::new(
            on_error.as_ref().unchecked_ref(),
            on_frame.as_ref().unchecked_ref(),
        );
        let inner = web_sys::VideoDecoder::new(&init)?;
        Ok(Self {
            inner,
            configured,
            timestamp: Cell::new(0),
            _on_frame: on_frame,
            _on_error: on_error,
        })
    }

    /// Feed one Annex B access unit.
    ///
    /// Everything before the first keyframe is dropped: a decoder configured
    /// from a mid-stream P-frame has no reference to decode against.
    pub fn decode(&self, payload: &[u8], width: u32, height: u32) {
        let keyframe = is_keyframe(payload);
        if !self.configured.get() {
            if !keyframe {
                return;
            }
            let Some(codec) = codec_string(payload) else {
                return;
            };
            let config = VideoDecoderConfig::new(&codec);
            config.set_coded_width(width);
            config.set_coded_height(height);
            // No `description`, which is what tells WebCodecs the bitstream is
            // Annex B rather than AVCC-framed.
            config.set_optimize_for_latency(true);
            if self.inner.configure(&config).is_err() {
                return;
            }
            self.configured.set(true);
        }

        let kind = if keyframe {
            EncodedVideoChunkType::Key
        } else {
            EncodedVideoChunkType::Delta
        };
        // Timestamps only have to increase: the compositor sends a frame when
        // something changed, not on a clock, so real presentation times would be
        // a fiction anyway.
        let timestamp = self.timestamp.get();
        self.timestamp.set(timestamp.wrapping_add(1000));
        let init = EncodedVideoChunkInit::new(&js_sys::Uint8Array::from(payload), timestamp, kind);
        init.set_duration(1000);
        let Ok(chunk) = EncodedVideoChunk::new(&init) else {
            return;
        };
        let _ = self.inner.decode(&chunk);
    }
}

/// Walk Annex B start codes, calling `f` with each NAL unit's first byte.
fn nal_units(payload: &[u8], mut f: impl FnMut(&[u8]) -> bool) {
    let mut i = 0;
    while i + 3 < payload.len() {
        // Start codes are 00 00 01 or 00 00 00 01; both end with 00 00 01.
        if payload[i] == 0 && payload[i + 1] == 0 && payload[i + 2] == 1 {
            let nal = &payload[i + 3..];
            if !nal.is_empty() && f(nal) {
                return;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
}

/// Does this access unit contain an IDR (NAL type 5)?
fn is_keyframe(payload: &[u8]) -> bool {
    let mut idr = false;
    nal_units(payload, |nal| {
        if nal[0] & 0x1F == 5 {
            idr = true;
            return true;
        }
        false
    });
    idr
}

/// Build the `avc1.PPCCLL` codec string out of the stream's own SPS.
///
/// Hardcoding this would be a guess: the encoder picks a level from the
/// resolution and bitrate it was given, so a 1280x800 surface and a 1080p one do
/// not agree, and a codec string below the real level can be rejected outright.
fn codec_string(payload: &[u8]) -> Option<String> {
    let mut out = None;
    nal_units(payload, |nal| {
        // SPS is type 7; profile_idc, constraint flags and level_idc are the
        // three bytes straight after the NAL header.
        if nal[0] & 0x1F == 7 && nal.len() >= 4 {
            out = Some(format!("avc1.{:02X}{:02X}{:02X}", nal[1], nal[2], nal[3]));
            return true;
        }
        false
    });
    out
}
