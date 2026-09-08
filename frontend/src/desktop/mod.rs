//! The Webland shell: panels, dock, launcher, window chrome.
//!
//! For now it is just the surface canvas plus the plumbing that connects the
//! backend transport to the renderer. The real shell (Phase 5) is written here
//! once the transport is proven.

use std::cell::RefCell;
use std::rc::Rc;

use leptos::html::Div;
use leptos::prelude::*;
use web_sys::Element;

use crate::latency::Latency;
use crate::protocol::{
    ClientMessage, ServerMessage, Transport, WebSocketTransport, decode, encode,
};
use crate::scene::Scene;

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

#[component]
pub fn Desktop() -> impl IntoView {
    let status = RwSignal::new(String::from("connecting…"));
    let scene_ref: NodeRef<Div> = NodeRef::new();

    // Connect only once the canvas is actually in the DOM (the ref fills on
    // mount, which re-runs this effect); avoids a cold-load race.
    Effect::new(move |_| {
        if let Some(container) = scene_ref.get() {
            connect_and_render(status, container.into());
        }
    });

    view! {
        <main>
            <p class="status">{move || status.get()}</p>
            <div node_ref=scene_ref id="webland-scene"></div>
        </main>
    }
}

fn connect_and_render(status: RwSignal<String>, container: Element) {
    let latency = Rc::new(Latency::new());
    let scene = Rc::new(RefCell::new(Scene::new(container.clone(), latency.clone())));

    let backend = backend();
    let transport = match WebSocketTransport::connect(&backend) {
        Ok(transport) => Rc::new(transport),
        Err(_) => {
            status.set(format!("could not open {backend}"));
            return;
        }
    };

    // Frames carry only what changed, so a mid-stream joiner needs one full
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
        status.set(String::from("disconnected — is the backend running?"));
    }));

    // Cloned into the handler so we can ack each presented frame (Decision 3:
    // the browser drives the frame clock). This Rc keeps the socket alive.
    let ack = transport.clone();
    let timing = latency.clone();
    let painting = scene.clone();
    transport.on_message(Box::new(move |bytes| {
        let Ok(message) = decode::<ServerMessage>(&bytes) else {
            return;
        };
        // Acks name their surface, so the compositor can pace each one on its
        // own rather than letting a busy window spend everyone's credit.
        let presented = match &message {
            ServerMessage::SurfaceFrame(frame) => Some(frame.id),
            _ => None,
        };
        painting.borrow_mut().handle(message);
        if let Some(id) = presented {
            status.set(timing.summary().unwrap_or_default());
            if let Ok(frame) = encode(&ClientMessage::FramePresented { id }) {
                ack.send(&frame);
            }
        }
    }));

    crate::input::wire(&container, transport, latency, scene);
}
