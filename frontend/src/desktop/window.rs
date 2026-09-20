//! One window: chrome the browser owns, wrapped around a canvas the compositor
//! fills.

use std::rc::Rc;

use leptos::html::{Canvas, Div};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::PointerEvent;
use webland_core::SurfaceId;
use webland_protocol::{ClientMessage, WindowRequest, encode};

use crate::protocol::WebSocketTransport;
use crate::scene::{ResizeDirection, Scene, SnapZone};

use super::menu::TitlebarMenu;
use super::resize::ResizeHandle;
use super::style;
use super::titlebar::Titlebar;

/// One window: chrome the browser owns, wrapped around a canvas the compositor
/// fills.
#[component]
pub fn Window(
    id: u64,
    scene: StoredValue<Scene, LocalStorage>,
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let send = move |message: &ClientMessage| {
        if let Some(transport) = transport.get_value()
            && let Ok(frame) = encode(message)
        {
            transport.send(&frame);
        }
    };
    let canvas_ref: NodeRef<Canvas> = NodeRef::new();

    // Attach a renderer once Leptos has actually put the canvas in the DOM, and
    // ask for a keyframe: anything sent before this had nowhere to land.
    Effect::new(move |_| {
        if let Some(canvas) = canvas_ref.get()
            && scene.with_value(|scene| scene.attach(id, &canvas))
        {
            send(&ClientMessage::RequestKeyframe);
        }
    });

    let set_maximized = move |maximize: bool| {
        if let Some(transport) = transport.get_value() {
            scene.with_value(|scene| scene.set_maximized_with_transport(id, maximize, &transport));
        }
    };

    // The same gestures, arriving from the client's own titlebar instead of the
    // shell's. A self-decorated window has no chrome here to click, so this is
    // the only way its buttons do anything at all.
    Effect::new(move |_| {
        let Some((target, request)) = scene.with_value(|scene| scene.requests.get()) else {
            return;
        };
        if target != id {
            return;
        }
        scene.with_value(|scene| scene.requests.set(None));
        match request {
            WindowRequest::Move => scene.with_value(|scene| {
                scene.raise(SurfaceId(id));
                scene.dragging.set(Some(id));
            }),
            WindowRequest::Maximize => set_maximized(true),
            WindowRequest::Unmaximize => set_maximized(false),
            WindowRequest::Minimize => scene.with_value(|scene| scene.set_minimized(id, true)),
        }
    });

    // Raising and focusing are one gesture: the browser stacks, the compositor
    // only learns who has the seat.
    //
    // Alt held makes it a move instead, which is the only way to shift a window
    // that has no titlebar here and whose client does not offer one either; a
    // client that decorates itself is assumed to, but nothing makes it.
    let focus = move |event: PointerEvent| {
        scene.with_value(|scene| scene.raise(SurfaceId(id)));
        if event.alt_key() {
            scene.with_value(|scene| scene.dragging.set(Some(id)));
            return;
        }
        send(&ClientMessage::Focus { id: SurfaceId(id) });

        let is_pointer_locked = scene.with_value(|scene| {
            scene.windows.with(|ws| {
                ws.iter()
                    .find(|w| w.id == id)
                    .is_some_and(|w| w.pointer_locked)
            })
        });
        if is_pointer_locked && let Some(canvas) = canvas_ref.get_untracked() {
            canvas.request_pointer_lock();
        }
    };

    let current = scene.with_value(|scene| scene.workspace);
    let cursor = scene.with_value(|scene| scene.cursor);
    let frame_ref: NodeRef<Div> = NodeRef::new();
    let style = move || style::frame(scene, id, current.get());
    let surface_style = move || style::surface(scene, id);
    let canvas_style = move || style::canvas(scene, id, &cursor.get());
    view! {
        // `bare` drops the shell's titlebar for a window that drew its own.
        //
        // ponytail: it is dropped for anything that never asked to be decorated,
        // which xdg-decoration says to read as "the client decorates itself";
        // true of GTK, and of nothing that draws no chrome at all. Such a window
        // keeps its resize handles and moves on alt-drag, but loses the shell's
        // close button; give the panel's task button a close if one turns up.
        <div node_ref=frame_ref class="window" style=style data-window=id.to_string()
             class:active=move || scene.with_value(|s| s.focused.get() == Some(id))
             class:bare=move || style::state(scene, id).is_some_and(|w| !w.decorated)
             class:popup=move || style::state(scene, id).is_some_and(|w| w.parent.is_some())
             class:maximized=move || style::state(scene, id).is_some_and(|w| w.restore.is_some() || w.snap == Some(SnapZone::Maximize))
             class:snapped=move || style::state(scene, id).is_some_and(|w| w.snap.is_some())
             on:pointerdown=move |_| {
                 scene.with_value(|s| s.raise(SurfaceId(id)));
                 send(&ClientMessage::Focus { id: SurfaceId(id) });
             }>
            <Titlebar id=id scene=scene transport=transport />
            // One bitmap pixel per *device* pixel. Dividing by the ratio is
            // what keeps a surface sharp: without it the browser stretches the
            // bitmap over `devicePixelRatio` screen pixels and softens it.
            <div class="surface" style=surface_style>
                <canvas node_ref=canvas_ref data-surface=id.to_string()
                        style=canvas_style on:pointerdown=focus></canvas>
            </div>
            <ResizeHandle id=id scene=scene transport=transport dir={ResizeDirection::Top} class="resize-top" />
            <ResizeHandle id=id scene=scene transport=transport dir={ResizeDirection::Bottom} class="resize-bottom" />
            <ResizeHandle id=id scene=scene transport=transport dir={ResizeDirection::Left} class="resize-left" />
            <ResizeHandle id=id scene=scene transport=transport dir={ResizeDirection::Right} class="resize-right" />
            <ResizeHandle id=id scene=scene transport=transport dir={ResizeDirection::TopLeft} class="resize-top-left" />
            <ResizeHandle id=id scene=scene transport=transport dir={ResizeDirection::TopRight} class="resize-top-right" />
            <ResizeHandle id=id scene=scene transport=transport dir={ResizeDirection::BottomLeft} class="resize-bottom-left" />
            <ResizeHandle id=id scene=scene transport=transport dir={ResizeDirection::BottomRight} class="resize-bottom-right" />
            <TitlebarMenu id=id scene=scene transport=transport />
        </div>
    }
}

/// Give the element a gesture started on the rest of that gesture, wherever the
/// pointer goes.
pub(super) fn capture(event: &PointerEvent) {
    if let Some(target) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    {
        let _ = target.set_pointer_capture(event.pointer_id());
    }
}
