//! Browser-side compositing (first cut).
//!
//! Draws `Raw` surface frames streamed from the backend onto a 2D canvas via
//! `putImageData`. WebGPU (see [`crate::gpu`]) is the eventual target — this
//! closes the loop visually and cheaply so the transport can be exercised end
//! to end before the GPU pipeline exists.

use wasm_bindgen::{Clamped, JsCast, JsValue};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData};
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
                self.size = Some((created.size.width, created.size.height));
                self.canvas.set_width(created.size.width);
                self.canvas.set_height(created.size.height);
            }
            ServerMessage::SurfaceFrame(frame) => self.draw(&frame),
        }
    }

    fn draw(&self, frame: &SurfaceFrame) {
        // Recover raw BGRA pixels. H.264 belongs to the WebCodecs path, not here.
        let raw = match frame.codec {
            Codec::Raw => frame.payload.clone(),
            Codec::Deflate => match inflate(&frame.payload) {
                Ok(bytes) => bytes,
                Err(_) => return,
            },
            Codec::H264 => return,
        };
        let Some((width, height)) = self.size else {
            return;
        };
        let expected = (width as usize) * (height as usize) * 4;
        if raw.len() < expected {
            return;
        }

        // wl_shm is little-endian ARGB, i.e. BGRA in memory; swizzle to RGBA for
        // ImageData. (Assumes tightly packed rows; stride/format land with the
        // protocol's per-frame metadata later.)
        let mut rgba = raw[..expected].to_vec();
        let mut i = 0;
        while i < rgba.len() {
            rgba.swap(i, i + 2);
            i += 4;
        }

        if let Ok(image) =
            ImageData::new_with_u8_clamped_array_and_sh(Clamped(&rgba), width, height)
        {
            let _ = self.ctx.put_image_data(&image, 0.0, 0.0);
        }
    }
}
