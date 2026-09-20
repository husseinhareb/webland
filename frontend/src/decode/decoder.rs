//! The `WebCodecs` decoder itself.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{
    CodecState, EncodedVideoChunk, EncodedVideoChunkInit, EncodedVideoChunkType,
    VideoDecoderConfig, VideoDecoderInit, VideoFrame,
};

use super::annexb::{codec_string, is_keyframe};

/// A `WebCodecs` decoder, configured from the first keyframe it is given.
pub struct Decoder {
    /// Replaceable, because a `VideoDecoder` that has reported an error is
    /// closed for good: see [`Decoder::reopen`].
    inner: RefCell<web_sys::VideoDecoder>,
    configured: Rc<Cell<bool>>,
    /// Set when there is nothing to decode until a keyframe arrives, and the
    /// compositor has not been asked for one yet.
    wants_keyframe: Rc<Cell<bool>>,
    timestamp: Cell<i32>,
    // Kept alive for as long as the decoder is: dropping a Closure detaches the
    // JS callback, and the decoder would then decode into nothing. They are
    // also what a replacement decoder is built from.
    on_frame: Closure<dyn FnMut(JsValue)>,
    on_error: Closure<dyn FnMut(JsValue)>,
}

impl Decoder {
    /// Build a decoder that hands each decoded frame to `on_frame`.
    ///
    /// # Errors
    /// Returns the JS error if the browser has no `VideoDecoder`, which is the
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
        let wants_keyframe = Rc::new(Cell::new(false));
        let failed = configured.clone();
        let asks = wants_keyframe.clone();
        let on_error = Closure::wrap(Box::new(move |value: JsValue| {
            web_sys::console::error_2(&JsValue::from_str("video decode failed"), &value);
            // The decoder is now closed and cannot be configured again, so this
            // only records that there is nothing to decode with: `decode` builds
            // a replacement. Asking for a keyframe is the other half; a new
            // decoder has no reference frames, and the compositor sends none
            // unless asked.
            failed.set(false);
            asks.set(true);
        }) as Box<dyn FnMut(JsValue)>);

        let init = VideoDecoderInit::new(
            on_error.as_ref().unchecked_ref(),
            on_frame.as_ref().unchecked_ref(),
        );
        let inner = web_sys::VideoDecoder::new(&init)?;
        Ok(Self {
            inner: RefCell::new(inner),
            configured,
            wants_keyframe,
            timestamp: Cell::new(0),
            on_frame,
            on_error,
        })
    }

    /// Whether the compositor should be asked for a keyframe, answered once per
    /// episode: the caller sends one message, not one per frame it drops.
    pub fn take_keyframe_request(&self) -> bool {
        self.wants_keyframe.replace(false)
    }

    /// Replace a decoder that has closed.
    ///
    /// A `VideoDecoder` that reports an error goes to `closed`, and everything
    /// on it throws from then on, including `configure`, so a decoder cannot
    /// recover itself. Nothing noticed, which is why a surface that hit one bad
    /// frame stayed black until the page was reloaded. The closures are the
    /// ones this decoder already owns, so the replacement reports to the same
    /// places.
    fn reopen(&self) {
        let init = VideoDecoderInit::new(
            self.on_error.as_ref().unchecked_ref(),
            self.on_frame.as_ref().unchecked_ref(),
        );
        if let Ok(fresh) = web_sys::VideoDecoder::new(&init) {
            *self.inner.borrow_mut() = fresh;
            self.configured.set(false);
            self.wants_keyframe.set(true);
        }
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
    pub fn decode(&self, payload: &[u8]) -> bool {
        if self.inner.borrow().state() == CodecState::Closed {
            self.reopen();
        }
        let keyframe = is_keyframe(payload);
        if !self.configured.get() {
            if !keyframe {
                // Nothing can be done with a delta frame here, and none of the
                // ones that follow it either, until the compositor sends a
                // keyframe to start again from.
                self.wants_keyframe.set(true);
                return false;
            }
            let Some(codec) = codec_string(payload) else {
                return false;
            };
            // No coded size: it is in the stream's own SPS, together with the
            // cropping that turns the encoder's macroblock-aligned picture back
            // into the surface. Passing the canvas's size here overrode both,
            // and the padding the crop was there to hide came out as a green
            // strip down the right of every surface whose width the encoder had
            // had to round.
            let config = VideoDecoderConfig::new(&codec);
            // No `description`, which is what tells WebCodecs the bitstream is
            // Annex B rather than AVCC-framed.
            config.set_optimize_for_latency(true);
            if self.inner.borrow().configure(&config).is_err() {
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
        self.inner.borrow().decode(&chunk).is_ok()
    }
}
