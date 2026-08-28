//! The Webland shell: panels, dock, launcher, window chrome.
//!
//! For now it is just the surface canvas plus the plumbing that connects the
//! backend transport to the renderer. The real shell (Phase 5) is written here
//! once the transport is proven.

use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlCanvasElement;

use crate::compositor::SurfaceRenderer;
use crate::protocol::{
    ClientMessage, ServerMessage, Transport, WebSocketTransport, decode, encode,
};

/// Backend WebSocket endpoint. Run the backend with `WEBLAND_WS=127.0.0.1:9001`
/// to match (localhost only until the protocol has authentication).
const BACKEND: &str = "ws://127.0.0.1:9001";

#[component]
pub fn Desktop() -> impl IntoView {
    let status = RwSignal::new(String::from("connecting…"));

    // After the canvas is in the DOM, connect and start rendering frames.
    Effect::new(move |_| connect_and_render(status));

    view! {
        <main>
            <p class="status">{move || status.get()}</p>
            <canvas id="webland-surface"></canvas>
        </main>
    }
}

fn connect_and_render(status: RwSignal<String>) {
    let Some(canvas) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("webland-surface"))
        .and_then(|element| element.dyn_into::<HtmlCanvasElement>().ok())
    else {
        return;
    };

    let renderer = match SurfaceRenderer::new(canvas.clone()) {
        Ok(renderer) => Rc::new(RefCell::new(renderer)),
        Err(_) => return,
    };

    let transport = match WebSocketTransport::connect(BACKEND) {
        Ok(transport) => Rc::new(transport),
        Err(_) => {
            status.set(format!("could not open {BACKEND}"));
            return;
        }
    };

    transport.on_open(Box::new(move || {
        status.set(String::from("connected — waiting for a surface…"));
    }));
    transport.on_close(Box::new(move || {
        status.set(format!(
            "disconnected — run the backend with WEBLAND_WS={}",
            BACKEND.trim_start_matches("ws://")
        ));
    }));

    // Cloned into the handler so we can ack each presented frame (Decision 3:
    // the browser drives the frame clock). This Rc keeps the socket alive.
    let ack = transport.clone();
    transport.on_message(Box::new(move |bytes| {
        let Ok(message) = decode::<ServerMessage>(&bytes) else {
            return;
        };
        let is_frame = matches!(message, ServerMessage::SurfaceFrame(_));
        if is_frame {
            status.set(String::new());
        }
        renderer.borrow_mut().handle(message);
        if is_frame && let Ok(frame) = encode(&ClientMessage::FramePresented) {
            ack.send(&frame);
        }
    }));

    // Stream browser input to the backend (Phase 3).
    crate::input::wire(&canvas, transport);
}
