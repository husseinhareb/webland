//! The browser-side scene: one window per Wayland surface.
//!
//! Phase 4 proved the streaming is per-surface; this is where that becomes a
//! desktop. Each surface has its own canvas, renderer and decoder — a separate
//! decoder is not a nicety, since each surface is an independent H.264 stream
//! with its own keyframes, and feeding two of them to one decoder produces
//! garbage from the first frame.
//!
//! Position, size and stacking live in [`WindowState`], which is a Leptos
//! signal: moving or raising a window rerenders a style attribute and tells the
//! compositor nothing at all.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use leptos::prelude::*;
use web_sys::HtmlCanvasElement;
use webland_core::SurfaceId;
use webland_protocol::{Application, Codec, ServerMessage, SurfaceFrame};

use crate::compositor::{Renderer, SurfaceRenderer};
use crate::decode::Decoder;
use crate::latency::Latency;

/// The browser's device pixel ratio, never zero.
#[must_use]
pub fn pixel_ratio() -> f64 {
    let ratio = web_sys::window().map_or(1.0, |window| window.device_pixel_ratio());
    if ratio > 0.0 { ratio } else { 1.0 }
}

/// Pixels each new window is offset from the last, so windows opening at the
/// same size do not land exactly on top of each other.
const CASCADE: i32 = 34;

/// Everything about a window that the shell draws, and nothing the compositor
/// needs to know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowState {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    /// Hidden, but still open and still streaming. The panel is how it comes
    /// back, which is why a minimized window keeps its task button.
    pub minimized: bool,
    /// Where the window sat before it was maximized — so `Some` is what it
    /// means to be maximized, and there is no way to be maximized with nowhere
    /// to go back to.
    pub restore: Option<(i32, i32)>,
}

/// What actually paints a surface, once its canvas exists in the DOM.
struct View {
    canvas: HtmlCanvasElement,
    renderer: Rc<RefCell<Renderer>>,
    decoder: Option<Decoder>,
}

/// The set of open windows.
#[derive(Clone)]
pub struct Scene {
    /// Drives the rendered window list.
    pub windows: RwSignal<Vec<WindowState>>,
    /// What the launcher may start, as the compositor reported it.
    pub applications: RwSignal<Vec<Application>>,
    views: Rc<RefCell<HashMap<u64, View>>>,
    /// The latest frame for a surface whose canvas Leptos has not mounted yet.
    ///
    /// Leptos mounts on its own schedule, so a surface's first frame regularly
    /// arrives before there is anywhere to put it. Asking for another keyframe
    /// works only if the timing happens to suit; keeping the frame always does,
    /// and one frame per surface is a bounded thing to hold.
    pending: Rc<RefCell<HashMap<u64, SurfaceFrame>>>,
    latency: Rc<Latency>,
    top: Rc<Cell<i32>>,
    opened: Rc<Cell<i32>>,
}

impl Scene {
    #[must_use]
    pub fn new(latency: Rc<Latency>) -> Self {
        Self {
            windows: RwSignal::new(Vec::new()),
            applications: RwSignal::new(Vec::new()),
            views: Rc::new(RefCell::new(HashMap::new())),
            pending: Rc::new(RefCell::new(HashMap::new())),
            latency,
            top: Rc::new(Cell::new(1)),
            opened: Rc::new(Cell::new(0)),
        }
    }

    /// Apply a message from the compositor to the window it names.
    pub fn handle(&self, message: ServerMessage) {
        match message {
            ServerMessage::SurfaceCreated(created) => {
                let id = created.id.0;
                let (width, height) = (created.size.width, created.size.height);
                if self.windows.with(|ws| ws.iter().any(|w| w.id == id)) {
                    // A resize, not a new window: the canvas follows the
                    // surface, and this is the only moment its bitmap should be
                    // reallocated — which also clears it, hence the keyframe
                    // that always accompanies a resize.
                    self.windows.update(|ws| {
                        if let Some(window) = ws.iter_mut().find(|w| w.id == id) {
                            window.width = width;
                            window.height = height;
                        }
                    });
                    if let Some(view) = self.views.borrow().get(&id)
                        && (view.canvas.width() != width || view.canvas.height() != height)
                    {
                        view.canvas.set_width(width);
                        view.canvas.set_height(height);
                    }
                    return;
                }
                let offset = self.opened.get() * CASCADE;
                self.opened.set(self.opened.get() + 1);
                self.top.set(self.top.get() + 1);
                self.windows.update(|ws| {
                    ws.push(WindowState {
                        id,
                        width,
                        height,
                        title: String::from("…"),
                        x: 40 + offset,
                        y: 40 + offset,
                        z: 0,
                        minimized: false,
                        restore: None,
                    });
                });
                self.raise(created.id);
            }
            ServerMessage::Applications(applications) => self.applications.set(applications),
            ServerMessage::SurfaceTitle { id, title } => {
                self.windows.update(|ws| {
                    if let Some(window) = ws.iter_mut().find(|w| w.id == id.0) {
                        window.title = title;
                    }
                });
            }
            ServerMessage::SurfaceFrame(frame) => {
                let views = self.views.borrow();
                let Some(view) = views.get(&frame.id.0) else {
                    self.pending.borrow_mut().insert(frame.id.0, frame);
                    return;
                };
                self.paint(view, frame);
            }
            ServerMessage::SurfaceDestroyed { id } => {
                self.views.borrow_mut().remove(&id.0);
                self.windows.update(|ws| ws.retain(|w| w.id != id.0));
            }
        }
    }

    /// Draw one frame into the view that owns it.
    fn paint(&self, view: &View, frame: SurfaceFrame) {
        if frame.codec == Codec::H264 {
            if let Some(decoder) = view.decoder.as_ref() {
                // The canvas bitmap, not the window state: the bitmap is by
                // definition what the client last rendered at, while the state's
                // size is the box on screen — which a resize grip drags ahead of
                // the client for the length of the gesture.
                decoder.decode(&frame.payload, view.canvas.width(), view.canvas.height());
            }
        } else {
            view.renderer
                .borrow_mut()
                .handle(ServerMessage::SurfaceFrame(frame));
            self.latency.frame_drawn();
        }
    }

    /// Give a window's canvas a renderer, once Leptos has mounted it.
    ///
    /// Returns `true` the first time a surface is attached, which is the moment
    /// to ask for a keyframe: frames that arrived before this had nowhere to go.
    pub fn attach(&self, id: u64, canvas: &HtmlCanvasElement) -> bool {
        if self.views.borrow().contains_key(&id) {
            return false;
        }
        // Size the bitmap here rather than from the view. Assigning `width` or
        // `height` clears a canvas even when the value does not change, and a
        // reactive attribute would do exactly that on the next render — wiping
        // the frame just drawn, with no new frame coming for an idle client.
        if let Some((width, height)) = self
            .windows
            .with_untracked(|ws| ws.iter().find(|w| w.id == id).map(|w| (w.width, w.height)))
        {
            canvas.set_width(width);
            canvas.set_height(height);
        }
        let Ok(canvas2d) = SurfaceRenderer::new(canvas.clone()) else {
            return false;
        };
        let renderer = Rc::new(RefCell::new(Renderer::Canvas(canvas2d)));
        let drawing = renderer.clone();
        let drawn = self.latency.clone();
        let decoder = Decoder::new(move |frame| {
            drawing.borrow_mut().draw_video_frame(frame);
            drawn.frame_drawn();
        })
        .ok();
        let view = View {
            canvas: canvas.clone(),
            renderer,
            decoder,
        };
        // Anything that arrived while this canvas was still being mounted.
        if let Some(frame) = self.pending.borrow_mut().remove(&id) {
            self.paint(&view, frame);
        }
        self.views.borrow_mut().insert(id, view);
        true
    }

    /// Put a window above the others. Browser state; the compositor is not told.
    pub fn raise(&self, id: SurfaceId) {
        self.top.set(self.top.get() + 1);
        let top = self.top.get();
        self.windows.update(|ws| {
            if let Some(window) = ws.iter_mut().find(|w| w.id == id.0) {
                window.z = top;
            }
        });
    }

    /// Move a window to a new top-left corner.
    pub fn move_to(&self, id: u64, x: i32, y: i32) {
        self.windows.update(|ws| {
            if let Some(window) = ws.iter_mut().find(|w| w.id == id) {
                window.x = x;
                window.y = y;
            }
        });
    }

    /// Hide a window, or bring it back. Browser state; the client goes on
    /// drawing, and never learns it is not being looked at.
    pub fn set_minimized(&self, id: u64, minimized: bool) {
        self.windows.update(|ws| {
            if let Some(window) = ws.iter_mut().find(|w| w.id == id) {
                window.minimized = minimized;
            }
        });
    }

    /// Send a window to the corner, or back where it came from.
    ///
    /// Only the corner: the size is the client's answer to the configure the
    /// caller sends, and arrives later as a fresh `SurfaceCreated`.
    pub fn set_maximized(&self, id: u64, maximized: bool) {
        self.windows.update(|ws| {
            let Some(window) = ws.iter_mut().find(|w| w.id == id) else {
                return;
            };
            match (maximized, window.restore) {
                (true, None) => {
                    window.restore = Some((window.x, window.y));
                    window.x = 0;
                    window.y = 0;
                }
                (false, Some((x, y))) => {
                    window.restore = None;
                    window.x = x;
                    window.y = y;
                }
                _ => {}
            }
        });
    }

    /// Set a window's box while a resize grip is being dragged.
    ///
    /// Only the box. The canvas bitmap keeps the size the client last rendered
    /// at, so CSS stretches the last frame for the length of the gesture — the
    /// client is told once, on release, and the sharp redraw comes back as a
    /// fresh `SurfaceCreated`.
    pub fn resize_to(&self, id: u64, width: u32, height: u32) {
        self.windows.update(|ws| {
            if let Some(window) = ws.iter_mut().find(|w| w.id == id) {
                window.width = width;
                window.height = height;
            }
        });
    }

    /// Whether a window is currently maximized.
    #[must_use]
    pub fn is_maximized(&self, id: u64) -> bool {
        self.windows
            .with_untracked(|ws| ws.iter().any(|w| w.id == id && w.restore.is_some()))
    }
}
