//! Reading the shell's own DOM: which window an element belongs to, which
//! canvas an event landed on, where inside a surface a coordinate falls, and
//! what the cursor should look like over it.

use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlCanvasElement, KeyboardEvent};
use webland_core::Point;

use crate::scene::{ResizeDirection, coord};

pub fn window_id_of_element(el: &Element) -> Option<u64> {
    el.closest(".window")
        .ok()
        .flatten()?
        .get_attribute("data-window")?
        .parse()
        .ok()
}

pub fn resize_dir_of_element(el: &Element) -> Option<ResizeDirection> {
    let handle = el.closest(".resize-handle").ok().flatten()?;
    let class = handle.class_name();
    if class.contains("resize-top-left") {
        Some(ResizeDirection::TopLeft)
    } else if class.contains("resize-top-right") {
        Some(ResizeDirection::TopRight)
    } else if class.contains("resize-bottom-left") {
        Some(ResizeDirection::BottomLeft)
    } else if class.contains("resize-bottom-right") {
        Some(ResizeDirection::BottomRight)
    } else if class.contains("resize-top") {
        Some(ResizeDirection::Top)
    } else if class.contains("resize-bottom") {
        Some(ResizeDirection::Bottom)
    } else if class.contains("resize-left") {
        Some(ResizeDirection::Left)
    } else if class.contains("resize-right") {
        Some(ResizeDirection::Right)
    } else {
        None
    }
}

/// Map a pointer event's canvas-local offset to surface pixels (the canvas may
/// be CSS-scaled, so scale by the ratio of backing size to displayed size).
pub fn surface_position(canvas: &HtmlCanvasElement, event: &web_sys::MouseEvent) -> Option<Point> {
    let displayed_w = f64::from(canvas.client_width());
    let displayed_h = f64::from(canvas.client_height());
    if displayed_w <= 0.0 || displayed_h <= 0.0 {
        return None;
    }
    Some(Point {
        // `offset_x`/`offset_y` are already f64 under web-sys's unstable cfg,
        // which the frontend builds with for WebCodecs.
        x: event.offset_x() * f64::from(canvas.width()) / displayed_w,
        y: event.offset_y() * f64::from(canvas.height()) / displayed_h,
    })
}

/// The element a click belongs to: the nearest one holding both the press and
/// the release, which is where the browser itself fires it. `None` when they
/// have nothing in common, which is a drag rather than a click.
pub fn clicked_element(up: &Element, down: &Element) -> Option<web_sys::HtmlElement> {
    let mut node = Some(up.clone());
    while let Some(el) = node {
        if el.contains(Some(down.as_ref())) {
            return el.dyn_into::<web_sys::HtmlElement>().ok();
        }
        node = el.parent_element();
    }
    None
}

/// Is this key event meant for the shell rather than an application?
///
/// Text typed into a form control belongs to the chrome around the surfaces, not
/// to the surfaces themselves. A button is not one of those: it takes no text,
/// and the browser leaves it focused after a click, so counting it here meant
/// that clicking the panel's task button to raise a window, or any of the
/// titlebar's, silently swallowed every keystroke afterwards. Return and space
/// would still have activated it, which `prevent_default` now stops; nothing
/// here reaches a button by keyboard anyway, since tab is prevented too.
pub fn aimed_at_shell(event: &KeyboardEvent) -> bool {
    let Some(target) = event.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
        return false;
    };
    matches!(
        target.tag_name().to_ascii_uppercase().as_str(),
        "INPUT" | "TEXTAREA" | "SELECT"
    )
}

/// The canvas an event landed on, if it landed on one at all.
pub fn target_canvas(event: &web_sys::Event) -> Option<HtmlCanvasElement> {
    event.target()?.dyn_into::<HtmlCanvasElement>().ok()
}

/// Extract the `SurfaceId` (as u64) from the canvas's `data-surface` DOM attribute.
pub fn canvas_surface_id(canvas: &HtmlCanvasElement) -> Option<u64> {
    canvas.get_attribute("data-surface")?.parse().ok()
}

/// Map viewport coordinates `(x, y)` to canvas-local surface pixel coordinates.
pub fn surface_position_at(canvas: &HtmlCanvasElement, x: f64, y: f64) -> Option<Point> {
    let rect = canvas.get_bounding_client_rect();
    let displayed_w = rect.width();
    let displayed_h = rect.height();
    if displayed_w <= 0.0 || displayed_h <= 0.0 {
        return None;
    }
    let offset_x = (x - rect.left()).clamp(0.0, displayed_w);
    let offset_y = (y - rect.top()).clamp(0.0, displayed_h);
    Some(Point {
        x: offset_x * f64::from(canvas.width()) / displayed_w,
        y: offset_y * f64::from(canvas.height()) / displayed_h,
    })
}

/// Dispatch a synthetic `PointerEvent` to an element (for captured desktop shell chrome interaction).
pub fn dispatch_pointer_event(
    target: &Element,
    event_type: &str,
    client_x: f64,
    client_y: f64,
    button: i16,
) {
    let init = web_sys::PointerEventInit::new();
    init.set_bubbles(true);
    init.set_cancelable(true);
    init.set_composed(true);
    init.set_client_x(coord(client_x));
    init.set_client_y(coord(client_y));
    init.set_screen_x(coord(client_x));
    init.set_screen_y(coord(client_y));
    init.set_button(button);
    init.set_buttons(u16::from(button == 0 && event_type != "pointerup"));
    init.set_pointer_id(1);
    init.set_pointer_type("mouse");
    if let Ok(event) = web_sys::PointerEvent::new_with_event_init_dict(event_type, &init) {
        let _ = target.dispatch_event(&event);
    }
}

/// Determine the cursor icon name for a given DOM element.
pub fn element_cursor_icon(el: &Element, guest_cursor: &str) -> &'static str {
    // 1. Resize handles:
    if let Ok(Some(handle)) = el.closest(".resize-handle") {
        let class = handle.class_name();
        if class.contains("resize-top-left") || class.contains("resize-bottom-right") {
            return "nwse-resize";
        }
        if class.contains("resize-top-right") || class.contains("resize-bottom-left") {
            return "nesw-resize";
        }
        if class.contains("resize-top") || class.contains("resize-bottom") {
            return "ns-resize";
        }
        if class.contains("resize-left") || class.contains("resize-right") {
            return "ew-resize";
        }
        return "nwse-resize";
    }

    // 2. Buttons and interactive clickable UI:
    if el
        .closest("button, a, input, select, textarea, .panel-btn, .task, .app, .menu-ws-btn, .alt-tab-item, .capture-release-btn, .toast-close")
        .is_ok_and(|opt| opt.is_some())
    {
        return "pointer";
    }

    // 3. Titlebar:
    if el.closest(".titlebar").is_ok_and(|opt| opt.is_some()) {
        return "grab";
    }

    // 4. Canvas / Surface:
    if el.tag_name().eq_ignore_ascii_case("CANVAS")
        || el.closest(".surface").is_ok_and(|opt| opt.is_some())
    {
        return match guest_cursor {
            "pointer" => "pointer",
            "text" => "text",
            "ns-resize" => "ns-resize",
            "ew-resize" => "ew-resize",
            "nwse-resize" => "nwse-resize",
            "nesw-resize" => "nesw-resize",
            "grab" => "grab",
            "grabbing" => "grabbing",
            _ => "default",
        };
    }

    "default"
}
