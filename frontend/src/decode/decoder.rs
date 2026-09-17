//! The `WebCodecs` decoder itself.

use std::cell::Cell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{
    EncodedVideoChunk, EncodedVideoChunkInit, EncodedVideoChunkType, VideoDecoderConfig,
    VideoDecoderInit, VideoFrame,
};

use super::annexb::{codec_string, is_keyframe};

/// A `WebCodecs` decoder, configured from the first keyframe it is given.
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
    ///
    /// Returns whether the chunk was queued, and so whether a `VideoFrame` is
    /// still to come for it. The caller needs to know, because presentation is
    /// what acks the frame to the compositor: a dropped chunk produces no
    /// output at all, and a surface waiting on one that will never arrive stops
    /// being sent frames.
    pub fn decode(&self, payload: &[u8], width: u32, height: u32) -> bool {
        let keyframe = is_keyframe(payload);
        if !self.configured.get() {
            if !keyframe {
                return false;
            }
            let Some(codec) = codec_string(payload) else {
                return false;
            };
            let config = VideoDecoderConfig::new(&codec);
            config.set_coded_width(width);
            config.set_coded_height(height);
            // No `description`, which is what tells WebCodecs the bitstream is
            // Annex B rather than AVCC-framed.
            config.set_optimize_for_latency(true);
            if self.inner.configure(&config).is_err() {
                return false;
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
            return false;
        };
        self.inner.decode(&chunk).is_ok()
    }
}
