//! Pointer input while the pointer is locked.
//!
//! With the lock taken the browser routes nothing: there is no target on the
//! event and no hover, so the shell tracks a virtual cursor and hit-tests
//! everything, its own chrome included, against `elementFromPoint`, then
//! dispatches synthetic pointer events at whatever it finds.
//!
//! The exception is a client holding a pointer constraint (a game grabbing the
//! mouse for its camera): relative motion goes straight through and no hit test
//! happens at all.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, HtmlCanvasElement, MouseEvent};
use webland_core::SurfaceId;
use webland_protocol::{ClientMessage, InputEvent, Press, encode};

use crate::latency::Latency;
use crate::protocol::WebSocketTransport;
use crate::scene::{self, Scene};

use super::dom::{
    canvas_surface_id, clicked_element, dispatch_pointer_event, element_cursor_icon,
    resize_dir_of_element, surface_position_at, window_id_of_element,
};
use super::keymap::evdev_button;
use super::pointer::{ClickState, LastClick, Pressed};
use super::wire::send;

/// Is a client holding the pointer for its own camera?
pub fn grabbed_by_client(scene: &Scene) -> bool {
    scene
        .windows
        .with(|ws| ws.iter().any(|w| w.pointer_locked && !w.minimized))
}

/// The element under the virtual cursor, if any.
fn element_at(doc: Option<&Document>, x: f64, y: f64) -> Option<web_sys::Element> {
    scene::element_at(doc?, x, y)
}

/// Point the virtual cursor at whatever is under it.
fn show_cursor_over(scene: &Scene, doc: Option<&Document>, x: f64, y: f64) {
    if let Some(el) = element_at(doc, x, y) {
        show_cursor(scene, &el);
    }
}

fn show_cursor(scene: &Scene, el: &web_sys::Element) {
    let guest = scene.cursor.get_untracked();
    scene
        .cursor_icon
        .set(element_cursor_icon(el, &guest).to_string());
}

/// Advance the virtual cursor by the event's relative motion, and act on where
/// it now is.
pub fn motion(
    scene: &Scene,
    transport: &WebSocketTransport,
    doc: Option<&Document>,
    event: &MouseEvent,
) {
    let dx = f64::from(event.movement_x());
    let dy = f64::from(event.movement_y());
    crate::dbg::log(format!(
        "DBG mousemove locked dx={dx} dy={dy} client=({},{}) screen=({},{}) dpr={}",
        event.client_x(),
        event.client_y(),
        event.screen_x(),
        event.screen_y(),
        web_sys::window().map_or(0.0, |w| w.device_pixel_ratio())
    ));
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    let (max_w, max_h) = viewport();
    let (cur_x, cur_y) = scene
        .virtual_cursor
        .get_untracked()
        .unwrap_or((max_w / 2.0, max_h / 2.0));
    let x = (cur_x + dx).clamp(0.0, max_w);
    let y = (cur_y + dy).clamp(0.0, max_h);
    scene.virtual_cursor.set(Some((x, y)));
    crate::dbg::log(format!(
        "DBG motion dx={dx} dy={dy} cur=({cur_x},{cur_y}) -> ({x},{y}) max=({max_w},{max_h})"
    ));

    if grabbed_by_client(scene) {
        send(transport, InputEvent::PointerMotionRelative { dx, dy });
        return;
    }
    if let Some((_id, resize)) = scene.resizing.get_untracked() {
        scene.update_active_resize(x, y);
        scene.cursor_icon.set(resize.dir.cursor().to_string());
        return;
    }
    if scene.titlebar_drag.get_untracked().is_some() {
        scene.update_titlebar_drag(x, y, transport);
        scene.cursor_icon.set(String::from("grabbing"));
        return;
    }
    let Some(el) = element_at(doc, x, y) else {
        return;
    };
    show_cursor(scene, &el);
    if let Ok(canvas) = el.dyn_into::<HtmlCanvasElement>()
        && let Some(id) = canvas_surface_id(&canvas)
        && let Some(position) = surface_position_at(&canvas, x, y)
    {
        send(
            transport,
            InputEvent::PointerMotion {
                id: SurfaceId(id),
                position,
            },
        );
    }
}

/// Let go of a gesture, or click what the virtual cursor is over.
pub fn release(
    scene: &Scene,
    transport: &WebSocketTransport,
    pressed: &Pressed,
    doc: Option<&Document>,
    event: &MouseEvent,
    (cx, cy): (f64, f64),
) {
    if scene.resizing.get_untracked().is_some() {
        scene.finish_active_resize(transport);
        show_cursor_over(scene, doc, cx, cy);
        return;
    }
    if scene.titlebar_drag.get_untracked().is_some() {
        scene.finish_titlebar_drag(cx, cy, transport);
        show_cursor_over(scene, doc, cx, cy);
        return;
    }
    let Some(el) = element_at(doc, cx, cy) else {
        return;
    };
    if el.dyn_ref::<HtmlCanvasElement>().is_some() {
        if let Some(button) = evdev_button(event.button()) {
            send(
                transport,
                InputEvent::PointerButton {
                    button,
                    state: Press::Up,
                },
            );
        }
        return;
    }
    dispatch_pointer_event(&el, "pointerup", cx, cy, event.button());
    // A click is a press *and* a release on the same thing. Firing it on
    // whatever the release landed on activated a button the press never
    // touched: a drag that began on the desktop and ended over the launcher
    // started an application. The browser fires it on the nearest element
    // holding both, so a press on a button's label and a release on the button
    // itself still counts.
    if let Some(down) = pressed.borrow_mut().take()
        && event.button() == 0
        && let Some(html_el) = clicked_element(&el, &down)
    {
        html_el.click();
    }
    show_cursor(scene, &el);
}

/// Press whatever the virtual cursor is over: shell chrome by hand, a client's
/// surface by forwarding the button.
pub fn press(
    scene: &Scene,
    transport: &WebSocketTransport,
    latency: &Latency,
    (last_click, pressed): (&LastClick, &Pressed),
    doc: Option<&Document>,
    event: &MouseEvent,
    (cx, cy): (f64, f64),
) {
    let Some(el) = element_at(doc, cx, cy) else {
        return;
    };
    // Every path below either handles the press itself or dispatches
    // `pointerdown`; only the latter records one, so a press the shell consumed
    // leaves nothing for a later release to click.
    pressed.borrow_mut().take();

    if titlebar_menu_took(scene, transport, &el) {
        return;
    }
    if let Ok(canvas) = el.clone().dyn_into::<HtmlCanvasElement>() {
        if let Some(id) = canvas_surface_id(&canvas) {
            focus(scene, transport, id);
        }
        if let Some(button) = evdev_button(event.button()) {
            latency.input_sent();
            send(
                transport,
                InputEvent::PointerButton {
                    button,
                    state: Press::Down,
                },
            );
        }
        return;
    }
    if let Some(id) = window_id_of_element(&el) {
        focus(scene, transport, id);
        if chrome_took(scene, transport, last_click, &el, event, id, (cx, cy)) {
            return;
        }
    }
    // Press only. A click is a press *and* a release, and the matching
    // `pointerup` is what fires it: clicking here too activated every button in
    // the shell twice, so toggles came straight back off and launchers opened
    // two copies of everything.
    pressed.borrow_mut().replace(el.clone());
    dispatch_pointer_event(&el, "pointerdown", cx, cy, event.button());
}

/// An open titlebar context menu swallows the press, whether or not it landed on
/// one of the menu's own items.
fn titlebar_menu_took(
    scene: &Scene,
    transport: &WebSocketTransport,
    el: &web_sys::Element,
) -> bool {
    let Some((id, _, _)) = scene.titlebar_menu.get_untracked() else {
        return false;
    };
    if el.closest(".titlebar-menu").ok().flatten().is_some() {
        if let Some(action_el) = el.closest("[data-action]").ok().flatten()
            && let Some(action) = action_el.get_attribute("data-action")
        {
            match action.as_str() {
                "minimize" => scene.set_minimized(id, true),
                "maximize" => scene.toggle_maximize(id, transport),
                "close" => close(transport, id),
                _ => {}
            }
            scene.titlebar_menu.set(None);
            return true;
        }
        if let Some(ws_el) = el.closest(".menu-ws-btn").ok().flatten()
            && let Some(ws) = ws_el
                .get_attribute("data-workspace")
                .and_then(|ws| ws.parse().ok())
        {
            scene.send_to_workspace(id, ws);
            scene.titlebar_menu.set(None);
            return true;
        }
    }
    scene.titlebar_menu.set(None);
    el.closest(".menu-backdrop").ok().flatten().is_some()
}

/// A press on the window's own chrome: a resize handle, a titlebar button, or
/// the titlebar itself. `true` if the chrome took it.
fn chrome_took(
    scene: &Scene,
    transport: &WebSocketTransport,
    last_click: &LastClick,
    el: &web_sys::Element,
    event: &MouseEvent,
    id: u64,
    (cx, cy): (f64, f64),
) -> bool {
    let on_titlebar = el.closest(".titlebar").ok().flatten().is_some();
    if event.button() == 2 && on_titlebar {
        scene.titlebar_menu.set(Some((id, cx, cy)));
        return true;
    }
    if event.button() != 0 {
        return false;
    }
    if let Some(dir) = resize_dir_of_element(el) {
        scene.start_active_resize(id, dir, cx, cy, transport);
        scene.cursor_icon.set(dir.cursor().to_string());
        return true;
    }
    if let Some(btn) = el.closest(".titlebar button").ok().flatten() {
        let class = btn.class_name();
        if class.contains("close") {
            close(transport, id);
        } else if class.contains("maximize") {
            scene.toggle_maximize(id, transport);
        } else if class.contains("minimize") {
            scene.set_minimized(id, true);
        }
        return true;
    }
    if !on_titlebar {
        return false;
    }
    let now = js_sys::Date::now();
    if last_click.borrow().as_ref().is_some_and(|last| {
        last.window_id == Some(id)
            && last.is_titlebar
            && (now - last.time) < 400.0
            && (cx - last.x).hypot(cy - last.y) < 16.0
    }) {
        last_click.borrow_mut().take();
        scene.toggle_maximize(id, transport);
    } else {
        last_click.borrow_mut().replace(ClickState {
            time: now,
            x: cx,
            y: cy,
            window_id: Some(id),
            is_titlebar: true,
        });
        scene.start_titlebar_drag(id, cx, cy, transport);
        scene.cursor_icon.set(String::from("grabbing"));
    }
    true
}

fn focus(scene: &Scene, transport: &WebSocketTransport, id: u64) {
    scene.raise(SurfaceId(id));
    if let Ok(frame) = encode(&ClientMessage::Focus { id: SurfaceId(id) }) {
        transport.send(&frame);
    }
}

fn close(transport: &WebSocketTransport, id: u64) {
    if let Ok(frame) = encode(&ClientMessage::CloseSurface { id: SurfaceId(id) }) {
        transport.send(&frame);
    }
}

/// The browser window's inner size, with a 1080p guess if it will not say.
pub fn viewport() -> (f64, f64) {
    let window = web_sys::window();
    let dimension = |value: Option<Result<wasm_bindgen::JsValue, _>>, fallback: f64| {
        value
            .and_then(Result::ok)
            .and_then(|v| v.as_f64())
            .unwrap_or(fallback)
    };
    (
        dimension(window.as_ref().map(web_sys::Window::inner_width), 1920.0),
        dimension(window.as_ref().map(web_sys::Window::inner_height), 1080.0),
    )
}
