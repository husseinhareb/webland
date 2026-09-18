//! The scene itself: the window list, the per-surface views that paint it, and
//! the frame path from wire to canvas to ack.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use leptos::prelude::*;
use web_sys::HtmlCanvasElement;
use webland_core::SurfaceId;
use webland_protocol::{
    Application, Codec, ServerMessage, SurfaceCreated, SurfaceFrame, WindowRequest,
};

use crate::compositor::SurfaceRenderer;
use crate::decode::Decoder;
use crate::latency::Latency;

use super::{ActiveResize, AltTabState, Grab, SnapZone, Toast, WindowState};

/// Pixels each new window is offset from the last, so windows opening at the
/// same size do not land exactly on top of each other.
const CASCADE: i32 = 34;

/// Told that a surface's frame is on screen, and whether that surface is one
/// the user can see. See [`Scene::on_presented`].
type Ack = Box<dyn Fn(SurfaceId, bool)>;

/// What actually paints a surface, once its canvas exists in the DOM.
pub(super) struct View {
    canvas: HtmlCanvasElement,
    renderer: Rc<RefCell<SurfaceRenderer>>,
    decoder: Option<Decoder>,
}

/// The set of open windows.
#[derive(Clone)]
pub struct Scene {
    /// Drives the rendered window list.
    pub windows: RwSignal<Vec<WindowState>>,
    /// What the launcher may start, as the compositor reported it.
    pub applications: RwSignal<Vec<Application>>,
    /// The workspace on screen. Windows on any other one are hidden.
    pub workspace: RwSignal<u32>,
    /// The pointer's look over a client's surface, as a CSS cursor keyword —
    /// the shell's own chrome keeps the cursors its stylesheet gives it.
    pub cursor: RwSignal<String>,
    /// A gesture a client made on its own titlebar, waiting for the window it
    /// names to act on it. One at a time: a pointer makes one gesture, and the
    /// window that takes it clears this.
    pub requests: RwSignal<Option<(u64, WindowRequest)>>,
    /// The window following the pointer because its client asked to be moved.
    /// The shell's own titlebar drag does not use this — it has the pointer
    /// events already, and this is for the case where the client has them.
    pub dragging: RwSignal<Option<u64>>,
    /// Whether the browser currently has pointer lock active (VM capture mode).
    pub captured: RwSignal<bool>,
    /// Whether the browser is currently in fullscreen mode.
    pub fullscreen: RwSignal<bool>,
    /// The virtual cursor position on screen (when captured and not hidden).
    pub virtual_cursor: RwSignal<Option<(f64, f64)>>,
    /// The currently active / focused window ID.
    pub focused: RwSignal<Option<u64>>,
    /// The snap preview zone currently targeted during a window drag.
    pub snap_preview: RwSignal<Option<SnapZone>>,
    /// Active state of the Alt+Tab window switcher modal.
    pub alt_tab: RwSignal<Option<AltTabState>>,
    /// Active system toast notifications.
    pub toasts: RwSignal<Vec<Toast>>,
    /// The active cursor icon shape (default, ns-resize, ew-resize, pointer, grab, etc.)
    pub cursor_icon: RwSignal<String>,
    /// Active window resize gesture: (`window_id`, `ActiveResize`)
    pub resizing: RwSignal<Option<(u64, ActiveResize)>>,
    /// Active window titlebar drag gesture: (`window_id`, Grab)
    pub titlebar_drag: RwSignal<Option<(u64, Grab)>>,
    /// Active window titlebar context menu: (`window_id`, `client_x`, `client_y`)
    pub titlebar_menu: RwSignal<Option<(u64, f64, f64)>>,
    pub(super) views: Rc<RefCell<HashMap<u64, View>>>,
    /// The latest frame for a surface whose canvas Leptos has not mounted yet.
    ///
    /// Leptos mounts on its own schedule, so a surface's first frame regularly
    /// arrives before there is anywhere to put it. Asking for another keyframe
    /// works only if the timing happens to suit; keeping the frame always does,
    /// and one frame per surface is a bounded thing to hold.
    pub(super) pending: Rc<RefCell<HashMap<u64, SurfaceFrame>>>,
    /// Told when a surface's frame has actually been put on screen, and whether
    /// that surface is one the user can see. This is what acks a frame to the
    /// compositor, so it must fire on presentation and not on arrival: see
    /// [`Scene::paint`].
    pub(super) acks: Rc<RefCell<Option<Ack>>>,
    pub(super) latency: Rc<Latency>,
    pub(super) top: Rc<Cell<i32>>,
    pub(super) opened: Rc<Cell<i32>>,
}

impl Scene {
    #[must_use]
    pub fn new(latency: Rc<Latency>) -> Self {
        Self {
            windows: RwSignal::new(Vec::new()),
            applications: RwSignal::new(Vec::new()),
            workspace: RwSignal::new(0),
            cursor: RwSignal::new(String::from("default")),
            requests: RwSignal::new(None),
            dragging: RwSignal::new(None),
            captured: RwSignal::new(false),
            fullscreen: RwSignal::new(false),
            virtual_cursor: RwSignal::new(None),
            focused: RwSignal::new(None),
            snap_preview: RwSignal::new(None),
            alt_tab: RwSignal::new(None),
            toasts: RwSignal::new(Vec::new()),
            cursor_icon: RwSignal::new(String::from("default")),
            resizing: RwSignal::new(None),
            titlebar_drag: RwSignal::new(None),
            titlebar_menu: RwSignal::new(None),
            views: Rc::new(RefCell::new(HashMap::new())),
            pending: Rc::new(RefCell::new(HashMap::new())),
            acks: Rc::new(RefCell::new(None)),
            latency,
            top: Rc::new(Cell::new(1)),
            opened: Rc::new(Cell::new(0)),
        }
    }

    /// Apply a message from the compositor to the window it names.
    pub fn handle(&self, message: ServerMessage) {
        match message {
            ServerMessage::SurfaceCreated(created) => self.opened(created),
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
            ServerMessage::Cursor { name } => self.cursor.set(name),
            // Straight onto the browser's clipboard: nothing in the shell wants
            // to hold a copy of it, and the point is to paste it elsewhere.
            ServerMessage::Clipboard { text } => crate::input::set_clipboard(&text),
            ServerMessage::SurfaceRequest { id, request } => {
                self.requests.set(Some((id.0, request)));
            }
            ServerMessage::PointerConstraint { id, locked } => {
                self.windows.update(|ws| {
                    if let Some(window) = ws.iter_mut().find(|w| w.id == id.0) {
                        window.pointer_locked = locked;
                    }
                });
                if locked {
                    if let Some(view) = self.views.borrow().get(&id.0) {
                        view.canvas.request_pointer_lock();
                    }
                } else if !self.captured.get()
                    && let Some(doc) = web_sys::window().and_then(|w| w.document())
                    && doc.pointer_lock_element().is_some()
                {
                    doc.exit_pointer_lock();
                }
            }
            ServerMessage::SurfaceDestroyed { id } => {
                self.views.borrow_mut().remove(&id.0);
                self.windows.update(|ws| ws.retain(|w| w.id != id.0));
                // A window that vanished mid-drag takes the drag with it.
                if self.dragging.get_untracked() == Some(id.0) {
                    self.dragging.set(None);
                }
                if self.focused.get_untracked() == Some(id.0) {
                    let current_ws = self.workspace.get_untracked();
                    let next_top = self.windows.with(|ws| {
                        ws.iter()
                            .filter(|w| w.workspace == current_ws && !w.minimized)
                            .max_by_key(|w| w.z)
                            .map(|w| w.id)
                    });
                    self.focused.set(next_top);
                }
            }
        }
    }

    /// Say that a surface's frame is now on screen, and ask for the next one.
    ///
    /// Called on presentation rather than on arrival. The browser drives the
    /// frame clock (Decision 3), and it can only do that by acking what it has
    /// actually drawn: acking on receipt told the compositor to send more while
    /// the decoder was still working through the last batch, which is an
    /// unbounded decode queue and exactly the backlog the clock exists to stop.
    pub(super) fn presented(&self, id: SurfaceId) {
        let visible = self.is_visible(id);
        if let Some(ack) = self.acks.borrow().as_ref() {
            ack(id, visible);
        }
    }

    /// Be told when a frame has been presented, so it can be acked.
    ///
    /// The second argument is whether the surface is on screen: a hidden window
    /// is still decoded and still acked, only far more slowly, and the pacing
    /// that decides how slowly belongs with the transport.
    pub fn on_presented(&self, ack: impl Fn(SurfaceId, bool) + 'static) {
        *self.acks.borrow_mut() = Some(Box::new(ack));
    }

    /// Draw one frame into the view that owns it.
    fn paint(&self, view: &View, frame: SurfaceFrame) {
        let id = frame.id;
        if frame.codec == Codec::H264 {
            // Decoding is asynchronous and finishes on the GPU later, so the
            // ack comes from the decoder's own callback — except when the chunk
            // was never queued, which produces no callback and would leave the
            // surface waiting forever.
            let queued = view.decoder.as_ref().is_some_and(|decoder| {
                // The canvas bitmap, not the window state: the bitmap is by
                // definition what the client last rendered at, while the state's
                // size is the box on screen — which a resize grip drags ahead of
                // the client for the length of the gesture.
                decoder.decode(&frame.payload, view.canvas.width(), view.canvas.height())
            });
            if !queued {
                self.presented(id);
            }
        } else {
            view.renderer
                .borrow_mut()
                .handle(ServerMessage::SurfaceFrame(frame));
            self.latency.frame_drawn();
            self.presented(id);
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
        let renderer = Rc::new(RefCell::new(canvas2d));
        let drawing = renderer.clone();
        let drawn = self.latency.clone();
        // A clone of the scene, held by the view it is about. The cycle is
        // broken where the view is: `SurfaceDestroyed` drops it, and with it the
        // decoder, its callback and this.
        let scene = self.clone();
        let decoder = Decoder::new(move |frame| {
            drawing.borrow_mut().draw_video_frame(frame);
            drawn.frame_drawn();
            // On screen now, and only now: this is the ack that asks the
            // compositor for the next frame of this surface.
            scene.presented(SurfaceId(id));
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

    /// A surface was announced: a new window, or one that changed size.
    fn opened(&self, created: SurfaceCreated) {
        let id = created.id.0;
        let (width, height) = (created.size.width, created.size.height);
        if self.windows.with(|ws| ws.iter().any(|w| w.id == id)) {
            // A resize, not a new window: the canvas follows the
            // surface, and this is the only moment its bitmap should be
            // reallocated — which also clears it, hence the keyframe
            // that always accompanies a resize.
            self.windows.update(|ws| {
                if let Some(window) = ws.iter_mut().find(|w| w.id == id) {
                    window.image = (width, height);
                    window.content = created.content;
                    // A menu that was repositioned is announced again
                    // at the same size, and hangs somewhere new.
                    window.parent = created.parent;
                    window.width = created.content.width;
                    window.height = created.content.height;
                    // Re-sent with every announce, and worth taking: a
                    // client that bound the decoration protocol late
                    // would otherwise keep the chrome it opened with.
                    window.decorated = created.decorated;
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
        // A popup is placed by its client, against the window that
        // opened it, so it takes no place in the cascade and none in
        // the stack: it draws above its parent, wherever that is.
        let offset = if created.parent.is_some() {
            0
        } else {
            let offset = self.opened.get() * CASCADE;
            self.opened.set(self.opened.get() + 1);
            offset
        };
        self.top.set(self.top.get() + 1);
        self.windows.update(|ws| {
            ws.push(WindowState {
                id,
                width: created.content.width,
                height: created.content.height,
                image: (width, height),
                content: created.content,
                title: String::from("…"),
                x: 40 + offset,
                y: 40 + offset,
                z: 0,
                minimized: false,
                parent: created.parent,
                decorated: created.decorated,
                // Where the user is looking. Launching something from
                // workspace 3 and having it open on 1 is the behaviour
                // nobody wants.
                workspace: self.workspace.get_untracked(),
                restore: None,
                pointer_locked: false,
                snap: None,
                floating: None,
            });
        });
        if created.parent.is_none() {
            self.raise(created.id);
        }
    }
}
