//! Browser-side compositing (first cut).
//!
//! Draws `Raw` surface frames streamed from the backend onto a 2D canvas via
//! `putImageData`. WebGPU (see [`crate::gpu`]) is the eventual target — this
//! closes the loop visually and cheaply so the transport can be exercised end
//! to end before the GPU pipeline exists.

use wasm_bindgen::{Clamped, JsCast, JsValue};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData, VideoFrame};
use webland_protocol::{Codec, ServerMessage, SurfaceFrame, inflate};

use crate::gpu::GpuRenderer;

/// The active render path: WebGPU when available, else the 2D canvas.
///
/// `Gpu` is unreachable until [`crate::gpu`] is wired back into the scene; see
/// the note there.
#[allow(dead_code)]
pub enum Renderer {
    Gpu(Box<GpuRenderer>),
    Canvas(SurfaceRenderer),
}

impl Renderer {
    /// Apply a server message to whichever renderer is active.
    pub fn handle(&mut self, message: ServerMessage) {
        match self {
            Renderer::Gpu(renderer) => renderer.handle(message),
            Renderer::Canvas(renderer) => renderer.handle(message),
        }
    }

    /// Draw a frame that came back from the video decoder.
    pub fn draw_video_frame(&mut self, frame: &VideoFrame) {
        match self {
            Renderer::Gpu(renderer) => renderer.draw_video_frame(frame),
            Renderer::Canvas(renderer) => renderer.draw_video_frame(frame),
        }
    }
}

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
            | ServerMessage::Applications(_) => {}
        }
    }

    /// Blit a decoded video frame. The browser does the colour conversion.
    pub fn draw_video_frame(&self, frame: &VideoFrame) {
        let _ = self.ctx.draw_image_with_video_frame(frame, 0.0, 0.0);
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
        let expected = (region.width as usize) * (region.height as usize) * 4;
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
