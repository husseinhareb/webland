//! The Webland shell: panels, dock, launcher, window chrome.
//!
//! For now it is just the surface canvas plus the plumbing that connects the
//! backend transport to the renderer. The real shell (Phase 5) is written here
//! once the transport is proven.

use std::cell::RefCell;
use std::rc::Rc;

use leptos::html::Canvas;
use leptos::prelude::*;
use web_sys::HtmlCanvasElement;

use crate::compositor::{Renderer, SurfaceRenderer};
use crate::gpu::GpuRenderer;
use crate::protocol::{
    ClientMessage, ServerMessage, Transport, WebSocketTransport, decode, encode,
};

/// Backend WebSocket endpoint. Run the backend with `WEBLAND_WS=127.0.0.1:9001`
/// to match (localhost only until the protocol has authentication).
const BACKEND: &str = "ws://127.0.0.1:9001";

#[component]
pub fn Desktop() -> impl IntoView {
    let status = RwSignal::new(String::from("connecting…"));
    let canvas_ref: NodeRef<Canvas> = NodeRef::new();

    // Connect only once the canvas is actually in the DOM (the ref fills on
    // mount, which re-runs this effect); avoids a cold-load race.
    Effect::new(move |_| {
        if let Some(canvas) = canvas_ref.get() {
            connect_and_render(status, canvas);
        }
    });

    view! {
        <main>
            <p class="status">{move || status.get()}</p>
            <canvas node_ref=canvas_ref id="webland-surface"></canvas>
        </main>
    }
}

fn connect_and_render(status: RwSignal<String>, canvas: HtmlCanvasElement) {
    // WebGPU init is async; do it (and the fallback) before wiring the socket.
    wasm_bindgen_futures::spawn_local(async move {
        let renderer = match GpuRenderer::new(canvas.clone()).await {
            Ok(gpu) => Renderer::Gpu(Box::new(gpu)),
            Err(err) => {
                // Name the fix: wgpu's own error says which stage failed but not
                // that both browsers gate WebGPU behind a setting on Linux.
                web_sys::console::warn_1(
                    &format!(
                        "WebGPU unavailable ({err}); using the 2D canvas. \
                         Firefox: set dom.webgpu.enabled in about:config. \
                         Chromium: enable chrome://flags/#enable-vulkan \
                         (or launch with --enable-features=Vulkan --use-angle=vulkan)."
                    )
                    .into(),
                );
                match SurfaceRenderer::new(canvas.clone()) {
                    Ok(canvas2d) => Renderer::Canvas(canvas2d),
                    Err(_) => {
                        status.set(String::from("no renderer available"));
                        return;
                    }
                }
            }
        };
        wire_transport(status, canvas, Rc::new(RefCell::new(renderer)));
    });
}

fn wire_transport(
    status: RwSignal<String>,
    canvas: HtmlCanvasElement,
    renderer: Rc<RefCell<Renderer>>,
) {
    let transport = match WebSocketTransport::connect(BACKEND) {
        Ok(transport) => Rc::new(transport),
        Err(_) => {
            status.set(format!("could not open {BACKEND}"));
            return;
        }
    };

    // Frames carry only damaged pixels, so a mid-stream joiner needs one full
    // surface to patch into; asking on connect is what keeps an idle desktop
    // from costing anything at all.
    let opened = transport.clone();
    transport.on_open(Box::new(move || {
        status.set(String::from("connected — waiting for a surface…"));
        if let Ok(frame) = encode(&ClientMessage::RequestKeyframe) {
            opened.send(&frame);
        }
    }));
    transport.on_close(Box::new(move || {
        status.set(format!(
            "disconnected — is the backend running on {}?",
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
