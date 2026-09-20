//! Browser-side compositing.
//!
//! Draws `Raw` and `Deflate` surface frames onto a 2D canvas via `putImageData`,
//! and blits decoded `VideoFrame`s straight from `WebCodecs`. The browser does the
//! colour conversion in both cases.

use wasm_bindgen::{Clamped, JsCast, JsValue};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData, VideoFrame};
use webland_protocol::{Codec, ServerMessage, SurfaceFrame, inflate};

/// Renders one surface's frames into a canvas.
#[derive(Debug)]
pub struct SurfaceRenderer {
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    size: Option<(u32, u32)>,
}

impl SurfaceRenderer {
    /// Wrap a canvas and grab its 2D context.
    ///
    /// # Errors
    /// Returns the JS error if the 2D context cannot be obtained.
    pub fn new(canvas: HtmlCanvasElement) -> Result<Self, JsValue> {
        let ctx = canvas
            .get_context("2d")?
            .ok_or_else(|| JsValue::from_str("2d context unavailable"))?
            .dyn_into::<CanvasRenderingContext2d>()?;
        Ok(Self {
            canvas,
            ctx,
            size: None,
        })
    }

    /// Apply a server message: resize on `SurfaceCreated`, draw on `SurfaceFrame`.
    pub fn handle(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::SurfaceCreated(created) => {
                let size = (created.size.width, created.size.height);
                // Only resize when it actually changes: setting width/height
                // clears the canvas, which would flicker on every re-announce.
                if self.size != Some(size) {
                    self.size = Some(size);
                    self.canvas.set_width(created.size.width);
                    self.canvas.set_height(created.size.height);
                }
            }
            ServerMessage::SurfaceFrame(frame) => self.draw(&frame),
            // The scene owns surface lifetime and chrome; a renderer only draws.
            ServerMessage::SurfaceDestroyed { .. }
            | ServerMessage::SurfaceTitle { .. }
            | ServerMessage::SurfaceRequest { .. }
            | ServerMessage::Cursor { .. }
            | ServerMessage::Clipboard { .. }
            | ServerMessage::PointerConstraint { .. }
            | ServerMessage::Applications(_) => {}
        }
    }

    /// Blit a decoded video frame. The browser does the colour conversion.
    ///
    /// Clipped to the frame's visible rectangle and no further than the canvas:
    /// an encoder rounds a surface up to whole macroblocks, and the padding it
    /// adds is undefined pixels — green, in practice, since empty NV12 decodes
    /// that way. The stream's crop is supposed to hide it and usually does, so
    /// this is the belt to that braces: nothing outside the surface's own box
    /// can reach the canvas whatever the decoder hands over.
    pub fn draw_video_frame(&self, frame: &VideoFrame) {
        let (canvas_w, canvas_h) = (self.canvas.width(), self.canvas.height());
        let visible = frame.visible_rect();
        let width = visible
            .as_ref()
            .map_or(f64::from(canvas_w), web_sys::DomRectReadOnly::width)
            .min(f64::from(canvas_w));
        let height = visible
            .as_ref()
            .map_or(f64::from(canvas_h), web_sys::DomRectReadOnly::height)
            .min(f64::from(canvas_h));
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let _ = self
            .ctx
            .draw_image_with_video_frame_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
                frame, 0.0, 0.0, width, height, 0.0, 0.0, width, height,
            );
    }

    fn draw(&self, frame: &SurfaceFrame) {
        // Recover raw BGRA pixels. H.264 belongs to the WebCodecs path, not here.
        let mut rgba = match frame.codec {
            Codec::Raw => frame.payload.clone(),
            Codec::Deflate => match inflate(&frame.payload) {
                Ok(bytes) => bytes,
                Err(_) => return,
            },
            Codec::H264 => return,
        };
        // Frames carry only what changed; the canvas keeps the rest.
        let Some(region) = frame.damage.first().copied() else {
            return;
        };
        if region.width == 0 || region.height == 0 {
            return;
        }
        // Checked, because `usize` is 32 bits on wasm32: a damage rectangle big
        // enough to overflow the product would wrap to a small `expected` and
        // quietly pass the length test below.
        let Some(expected) = (region.width as usize)
            .checked_mul(region.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
        else {
            return;
        };
        if rgba.len() < expected {
            return;
        }
        rgba.truncate(expected);

        // wl_shm is little-endian ARGB, i.e. BGRA in memory; swizzle in place to
        // RGBA for ImageData. (Assumes tightly packed rows; stride/format land
        // with the protocol's per-frame metadata later.)
        let mut i = 0;
        while i < rgba.len() {
            rgba.swap(i, i + 2);
            i += 4;
        }

        if let Ok(image) =
            ImageData::new_with_u8_clamped_array_and_sh(Clamped(&rgba), region.width, region.height)
        {
            let _ = self.ctx.put_image_data(&image, region.x, region.y);
        }
    }
}
