//! The gestures that change a window's box and have to tell the compositor
//! about it: maximize, snap, resize drag, titlebar drag.

use leptos::prelude::*;
use webland_core::{Size, SurfaceId};
use webland_protocol::{ClientMessage, encode};

use crate::protocol::WebSocketTransport;

use super::{
    ActiveResize, MIN_SURFACE, ResizeDirection, Scene, SnapZone, coord, desktop_bounds,
    drag_origin, maximized_size, pixel_ratio, pixels, whole_blocks, workspace_at,
};

/// How close to an edge a titlebar drag has to get before it offers to snap.
const EDGE_THRESHOLD: f64 = 15.0;

impl Scene {
    /// Maximize or restore a window and send the corresponding protocol message.
    pub fn set_maximized_with_transport(
        &self,
        id: u64,
        maximize: bool,
        transport: &WebSocketTransport,
    ) {
        if maximize {
            self.set_maximized(id, true);
            if let Ok(frame) = encode(&ClientMessage::SetMaximized {
                id: SurfaceId(id),
                size: Some(maximized_size()),
            }) {
                transport.send(&frame);
            }
        } else {
            let floating = self.unsnap(id);
            if let Ok(frame) = encode(&ClientMessage::SetMaximized {
                id: SurfaceId(id),
                size: None,
            }) {
                transport.send(&frame);
            }
            if let Some((_, _, w, h)) = floating
                && let Ok(frame) = encode(&ClientMessage::SetSize {
                    id: SurfaceId(id),
                    size: Size {
                        width: w,
                        height: h,
                    },
                })
            {
                transport.send(&frame);
            }
        }
    }

    /// Toggle a window's maximized state.
    pub fn toggle_maximize(&self, id: u64, transport: &WebSocketTransport) {
        let is_snapped_or_max = self.is_snapped(id) || self.is_maximized(id);
        self.set_maximized_with_transport(id, !is_snapped_or_max, transport);
    }

    /// Snap a window to a half or maximize.
    pub fn apply_snap(&self, id: u64, zone: SnapZone, transport: &WebSocketTransport) {
        let (vw, vh) = desktop_bounds();
        let ratio = pixel_ratio();
        match zone {
            SnapZone::Maximize => {
                self.set_maximized_with_transport(id, true, transport);
            }
            SnapZone::Left => {
                let half_w = whole_blocks(pixels(vw * 0.5 * ratio));
                let full_h = whole_blocks(pixels(vh * ratio));
                self.snap_to(id, SnapZone::Left, half_w, full_h, 0, 0);
                if let Ok(frame) = encode(&ClientMessage::SetMaximized {
                    id: SurfaceId(id),
                    size: None,
                }) {
                    transport.send(&frame);
                }
                if let Ok(frame) = encode(&ClientMessage::SetSize {
                    id: SurfaceId(id),
                    size: Size {
                        width: half_w,
                        height: full_h,
                    },
                }) {
                    transport.send(&frame);
                }
            }
            SnapZone::Right => {
                let half_w = whole_blocks(pixels(vw * 0.5 * ratio));
                let full_h = whole_blocks(pixels(vh * ratio));
                let x = coord(vw * 0.5);
                self.snap_to(id, SnapZone::Right, half_w, full_h, x, 0);
                if let Ok(frame) = encode(&ClientMessage::SetMaximized {
                    id: SurfaceId(id),
                    size: None,
                }) {
                    transport.send(&frame);
                }
                if let Ok(frame) = encode(&ClientMessage::SetSize {
                    id: SurfaceId(id),
                    size: Size {
                        width: half_w,
                        height: full_h,
                    },
                }) {
                    transport.send(&frame);
                }
            }
        }
    }

    /// Begin a resize gesture for a window.
    pub fn start_active_resize(
        &self,
        id: u64,
        dir: ResizeDirection,
        from_x: f64,
        from_y: f64,
        transport: &WebSocketTransport,
    ) {
        if self.is_maximized(id) || self.is_snapped(id) {
            return;
        }
        self.raise(SurfaceId(id));
        let (orig_width, orig_height) = self.window_size_of(id);
        let (orig_x, orig_y) = self.window_origin(id);
        self.resizing.set(Some((
            id,
            ActiveResize {
                dir,
                from_x,
                from_y,
                orig_x,
                orig_y,
                orig_width,
                orig_height,
            },
        )));
        if let Ok(frame) = encode(&ClientMessage::Focus { id: SurfaceId(id) }) {
            transport.send(&frame);
        }
    }

    /// Update a window's size and position during a resize gesture.
    pub fn update_active_resize(&self, current_x: f64, current_y: f64) {
        let Some((id, resize)) = self.resizing.get_untracked() else {
            return;
        };
        let ratio = pixel_ratio();
        let dx = (current_x - resize.from_x) * ratio;
        let dy = (current_y - resize.from_y) * ratio;

        let mut new_width = resize.orig_width;
        let mut new_height = resize.orig_height;
        let mut new_x = resize.orig_x;
        let mut new_y = resize.orig_y;

        match resize.dir {
            ResizeDirection::Right | ResizeDirection::TopRight | ResizeDirection::BottomRight => {
                new_width = pixels((f64::from(resize.orig_width) + dx).max(MIN_SURFACE));
            }
            ResizeDirection::Left | ResizeDirection::TopLeft | ResizeDirection::BottomLeft => {
                let raw_w = (f64::from(resize.orig_width) - dx).max(MIN_SURFACE);
                new_width = pixels(raw_w);
                let actual_delta_w = raw_w - f64::from(resize.orig_width);
                new_x = resize.orig_x - coord(actual_delta_w / ratio);
            }
            ResizeDirection::Top | ResizeDirection::Bottom => {}
        }

        match resize.dir {
            ResizeDirection::Bottom
            | ResizeDirection::BottomLeft
            | ResizeDirection::BottomRight => {
                new_height = pixels((f64::from(resize.orig_height) + dy).max(MIN_SURFACE));
            }
            ResizeDirection::Top | ResizeDirection::TopLeft | ResizeDirection::TopRight => {
                let raw_h = (f64::from(resize.orig_height) - dy).max(MIN_SURFACE);
                new_height = pixels(raw_h);
                let actual_delta_h = raw_h - f64::from(resize.orig_height);
                new_y = resize.orig_y - coord(actual_delta_h / ratio);
            }
            ResizeDirection::Left | ResizeDirection::Right => {}
        }

        self.resize_and_move_to(id, new_width, new_height, new_x, new_y);
    }

    /// Finish an active resize gesture, committing the new size to the compositor.
    pub fn finish_active_resize(&self, transport: &WebSocketTransport) {
        let mut finished = None;
        self.resizing.update(|r| finished = r.take());
        if let Some((id, _)) = finished {
            let (width, height) = self.window_size_of(id);
            // Snapped to whole macroblocks at the moment the drag ends: the
            // client is asked for a size it can be encoded at exactly, and the
            // window's own box takes the same size so the shell is not left
            // scaling the surface by a few pixels.
            let (width, height) = (whole_blocks(width), whole_blocks(height));
            self.windows.update(|ws| {
                if let Some(window) = ws.iter_mut().find(|w| w.id == id) {
                    window.width = width;
                    window.height = height;
                }
            });
            if let Ok(frame) = encode(&ClientMessage::SetSize {
                id: SurfaceId(id),
                size: Size { width, height },
            }) {
                transport.send(&frame);
            }
        }
    }

    /// Begin dragging a window by its titlebar.
    pub fn start_titlebar_drag(&self, id: u64, cx: f64, cy: f64, transport: &WebSocketTransport) {
        self.raise(SurfaceId(id));
        let (x, y) = self.window_origin(id);
        let dx = cx - f64::from(x);
        let dy = cy - f64::from(y);
        self.titlebar_drag.set(Some((id, (dx, dy, x, y))));
        if let Ok(frame) = encode(&ClientMessage::Focus { id: SurfaceId(id) }) {
            transport.send(&frame);
        }
    }

    /// Update a window's position during a titlebar drag gesture.
    pub fn update_titlebar_drag(&self, cx: f64, cy: f64, transport: &WebSocketTransport) {
        let Some((id, (dx, dy, _orig_x, _orig_y))) = self.titlebar_drag.get_untracked() else {
            return;
        };
        let (vw, _) = desktop_bounds();

        if self.is_snapped(id)
            && cy > 12.0
            && let Some((orig_fx, orig_fy, orig_fw, orig_fh)) = self.unsnap(id)
        {
            if let Ok(frame) = encode(&ClientMessage::SetMaximized {
                id: SurfaceId(id),
                size: None,
            }) {
                transport.send(&frame);
            }
            if let Ok(frame) = encode(&ClientMessage::SetSize {
                id: SurfaceId(id),
                size: Size {
                    width: orig_fw,
                    height: orig_fh,
                },
            }) {
                transport.send(&frame);
            }
            // Re-grab the window under the pointer: it just changed size, and
            // the old offset would put the titlebar somewhere it is not.
            let css_w = f64::from(orig_fw) / pixel_ratio();
            let grab = ((css_w / 2.0).min(cx).max(20.0), 14.0);
            self.titlebar_drag
                .set(Some((id, (grab.0, grab.1, orig_fx, orig_fy))));
            let (x, y) = drag_origin((cx, cy), grab);
            self.move_to(id, x, y);
            return;
        }

        let (x, y) = drag_origin((cx, cy), (dx, dy));
        self.move_to(id, x, y);

        let snap_zone = if cy <= EDGE_THRESHOLD {
            Some(SnapZone::Maximize)
        } else if cx <= EDGE_THRESHOLD {
            Some(SnapZone::Left)
        } else if cx >= vw - EDGE_THRESHOLD {
            Some(SnapZone::Right)
        } else {
            None
        };
        self.snap_preview.set(snap_zone);
    }

    /// Finish a titlebar drag gesture.
    pub fn finish_titlebar_drag(&self, cx: f64, cy: f64, transport: &WebSocketTransport) {
        let mut held = None;
        self.titlebar_drag.update(|d| held = d.take());
        let mut preview = None;
        self.snap_preview.update(|p| preview = p.take());
        let Some((id, (_, _, from_x, from_y))) = held else {
            return;
        };
        if let Some(workspace) = workspace_at(cx, cy) {
            self.move_to(id, from_x, from_y);
            self.send_to_workspace(id, workspace);
        } else if let Some(zone) = preview {
            self.apply_snap(id, zone, transport);
        }
    }
}
