//! The browser-side scene: one view per Wayland surface.
//!
//! Phase 4's test is whether Phase 2 was per-surface for real, so each surface
//! gets its own `<canvas>`, its own renderer and its own decoder. Nothing here
//! is shared between surfaces except the page they sit on, which means position
//! and stacking are CSS and moving a window costs the server nothing — no round
//! trip, no re-encode, which is exactly what the phase asks for.
//!
//! A separate decoder per surface is not a nicety: each surface is an
//! independent H.264 stream with its own keyframes and reference frames, and
//! feeding two of them to one decoder produces garbage from the first frame.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlCanvasElement};
use webland_core::SurfaceId;
use webland_protocol::{Codec, ServerMessage};

use crate::compositor::{Renderer, SurfaceRenderer};
use crate::decode::Decoder;
use crate::latency::Latency;

/// Pixels each new surface is offset from the last, so three windows opening at
/// the same size do not land exactly on top of each other.
const CASCADE: i32 = 32;

struct View {
    canvas: HtmlCanvasElement,
    renderer: Rc<RefCell<Renderer>>,
    decoder: Option<Decoder>,
}

/// Every surface the browser currently knows about.
pub struct Scene {
    container: Element,
    views: HashMap<u64, View>,
    latency: Rc<Latency>,
    /// Bumped so a newly focused surface can be raised above the rest.
    top: Rc<RefCell<i32>>,
    opened: i32,
}

impl Scene {
    #[must_use]
    pub fn new(container: Element, latency: Rc<Latency>) -> Self {
        Self {
            container,
            views: HashMap::new(),
            latency,
            top: Rc::new(RefCell::new(1)),
            opened: 0,
        }
    }

    /// Apply a message from the compositor to the surface it names.
    pub fn handle(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::SurfaceCreated(created) => {
                self.ensure(created.id, created.size.width, created.size.height);
                if let Some(view) = self.views.get(&created.id.0) {
                    view.renderer
                        .borrow_mut()
                        .handle(ServerMessage::SurfaceCreated(created));
                }
            }
            ServerMessage::SurfaceFrame(frame) => {
                let Some(view) = self.views.get(&frame.id.0) else {
                    return;
                };
                if frame.codec == Codec::H264 {
                    if let Some(decoder) = view.decoder.as_ref() {
                        decoder.decode(&frame.payload, view.canvas.width(), view.canvas.height());
                    }
                } else {
                    view.renderer
                        .borrow_mut()
                        .handle(ServerMessage::SurfaceFrame(frame));
                    self.latency.frame_drawn();
                }
            }
            ServerMessage::SurfaceDestroyed { id } => {
                if let Some(view) = self.views.remove(&id.0) {
                    view.canvas.remove();
                }
            }
        }
    }

    /// Create the canvas and renderer for a surface we have not seen before.
    fn ensure(&mut self, id: SurfaceId, width: u32, height: u32) {
        if self.views.contains_key(&id.0) {
            return;
        }
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let Ok(element) = document.create_element("canvas") else {
            return;
        };
        let Ok(canvas) = element.dyn_into::<HtmlCanvasElement>() else {
            return;
        };
        canvas.set_width(width);
        canvas.set_height(height);
        let offset = self.opened * CASCADE;
        self.opened += 1;
        let _ = canvas.set_attribute(
            "style",
            &format!("position:absolute; left:{offset}px; top:{offset}px; z-index:1;"),
        );
        let _ = canvas.set_attribute("data-surface", &id.0.to_string());
        let _ = self.container.append_child(&canvas);

        // WebGPU per surface would want one device shared between them; this
        // asks for a device each.
        // ponytail: a handful of windows is a handful of devices, which the
        // browser tolerates. Share one `wgpu::Device` across surfaces when the
        // window count stops being a handful.
        let renderer = match SurfaceRenderer::new(canvas.clone()) {
            Ok(canvas2d) => Rc::new(RefCell::new(Renderer::Canvas(canvas2d))),
            Err(_) => return,
        };
        let drawing = renderer.clone();
        let drawn = self.latency.clone();
        let decoder = Decoder::new(move |frame| {
            drawing.borrow_mut().draw_video_frame(frame);
            drawn.frame_drawn();
        })
        .ok();

        self.views.insert(
            id.0,
            View {
                canvas,
                renderer,
                decoder,
            },
        );
    }

    /// Raise a surface above the others. Pure browser state — the compositor is
    /// never told, which is the point of the phase.
    pub fn raise(&self, id: SurfaceId) {
        let Some(view) = self.views.get(&id.0) else {
            return;
        };
        let mut top = self.top.borrow_mut();
        *top += 1;
        let style = view.canvas.get_attribute("style").unwrap_or_default();
        let style = strip_z_index(&style);
        let _ = view
            .canvas
            .set_attribute("style", &format!("{style} z-index:{top};"));
    }
}

/// Drop any `z-index` from an inline style so a new one can replace it.
fn strip_z_index(style: &str) -> String {
    style
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty() && !part.starts_with("z-index"))
        .map(|part| format!("{part};"))
        .collect::<Vec<_>>()
        .join(" ")
}
