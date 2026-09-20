//! Opening the socket and wiring it to the scene.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use webland_protocol::{ClientMessage, ServerMessage, decode, encode};

use crate::latency::Latency;
use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

use super::shell::window_size;

/// Path the protocol socket is served on, proxied to the backend by whatever is
/// serving the page (see `Trunk.toml`).
const BACKEND_PATH: &str = "/ws";

/// The protocol socket's URL, on the same origin as the page.
///
/// Derived rather than hardcoded so the page works wherever it is served from:
/// a hardcoded `127.0.0.1` is the viewer's own loopback once the browser is on
/// another machine, and `wss` is required on an https page.
fn backend() -> String {
    let Some(location) = web_sys::window().map(|window| window.location()) else {
        return format!("ws://127.0.0.1:9001{BACKEND_PATH}");
    };
    let secure = location.protocol().is_ok_and(|scheme| scheme == "https:");
    let scheme = if secure { "wss" } else { "ws" };
    let host = location
        .host()
        .unwrap_or_else(|_| String::from("127.0.0.1:3030"));
    format!("{scheme}://{host}{BACKEND_PATH}")
}

/// Open the transport and wire it to the scene.
pub fn connect(
    status: RwSignal<String>,
    scene: Scene,
    latency: Rc<Latency>,
) -> Option<Rc<WebSocketTransport>> {
    let backend = backend();
    let transport = if let Ok(transport) = WebSocketTransport::connect(&backend) {
        Rc::new(transport)
    } else {
        status.set(format!("could not open {backend}"));
        return None;
    };

    // Frames carry only what changed, so a mid-stream joiner needs one full
    // surface to patch into.
    let opened = transport.clone();
    transport.on_open(Box::new(move || {
        status.set(String::from("connected, waiting for a surface…"));
        // Say how big the display is before asking for anything to put on it,
        // so the first frame arrives at the right size rather than at a guess.
        if let Some(size) = window_size()
            && let Ok(frame) = encode(&ClientMessage::Resize { size })
        {
            opened.send(&frame);
        }
        if let Ok(frame) = encode(&ClientMessage::RequestKeyframe) {
            opened.send(&frame);
        }
        // And that this keyboard holds nothing, which the seat cannot know:
        // it keeps whatever the last page left pressed.
        crate::input::release_modifiers(&opened);
    }));

    // Follow the window: the browser is the display, so its size is the screen
    // resolution and a resized window is a mode change.
    {
        let resizing = transport.clone();
        let listener = Closure::<dyn FnMut()>::new(move || {
            if let Some(size) = window_size()
                && let Ok(frame) = encode(&ClientMessage::Resize { size })
            {
                resizing.send(&frame);
            }
        });
        if let Some(window) = web_sys::window() {
            let _ = window
                .add_event_listener_with_callback("resize", listener.as_ref().unchecked_ref());
        }
        listener.forget();
    }
    transport.on_close(Box::new(move || {
        status.set(String::from("disconnected; is the backend running?"));
    }));

    // Acks are sent when the scene says a frame is on screen, not when one
    // comes off the wire. H.264 decoding finishes on the GPU well after the
    // packet arrives, and acking on arrival told the compositor to send more
    // while the decoder was still behind; the browser is supposed to drive the
    // frame clock (Decision 3), and it cannot do that by acking work it has not
    // done.
    {
        let ack = transport.clone();
        let hidden_acks = Rc::new(RefCell::new(HashMap::<u64, f64>::new()));
        scene.on_presented(move |id, visible| {
            let should_ack = if visible {
                hidden_acks.borrow_mut().remove(&id.0);
                true
            } else {
                // Off-screen frame pacing throttling (Roadmap Risk #5):
                // Minimized windows and windows on inactive workspaces are
                // throttled to an idle presentation rate (500ms / 2 FPS). The
                // compositor's per-surface FrameClock withholds
                // wl_surface.frame callbacks, eliminating wasted GPU
                // encode/decode for hidden surfaces.
                let now = web_sys::window()
                    .and_then(|w| w.performance())
                    .map_or_else(js_sys::Date::now, |p| p.now());
                let mut acks = hidden_acks.borrow_mut();
                let last = acks.get(&id.0).copied().unwrap_or(0.0);
                if now - last >= 500.0 {
                    acks.insert(id.0, now);
                    true
                } else {
                    false
                }
            };
            if should_ack && let Ok(frame) = encode(&ClientMessage::FramePresented { id }) {
                ack.send(&frame);
            }
        });
    }

    {
        let asking = transport.clone();
        scene.on_keyframe_wanted(move || {
            if let Ok(frame) = encode(&ClientMessage::RequestKeyframe) {
                asking.send(&frame);
            }
        });
    }

    // Built once, whatever the browser allows: a player that cannot play is
    // `None` and the desktop is simply silent.
    let player = crate::audio::Player::new();

    let ack = transport.clone();
    transport.on_message(Box::new(move |bytes| {
        let Ok(message) = decode::<ServerMessage>(&bytes) else {
            return;
        };
        // Audio belongs to the page, not to the scene: no window owns it and
        // nothing about it is drawn.
        if let ServerMessage::Audio { payload } = message {
            if let Some(player) = player.as_ref() {
                player.push(payload);
            }
            return;
        }
        if let ServerMessage::SurfaceCreated(ref created) = message
            && created.parent.is_none()
            && let Ok(frame) = encode(&ClientMessage::Focus { id: created.id })
        {
            ack.send(&frame);
        }
        let is_frame = matches!(message, ServerMessage::SurfaceFrame(_));
        // The ack for this frame comes back through `on_presented` above, once
        // the pixels are actually drawn.
        scene.handle(message);
        if is_frame {
            status.set(latency.summary().unwrap_or_default());
        }
    }));

    Some(transport)
}
