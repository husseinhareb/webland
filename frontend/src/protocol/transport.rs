//! WebSocket transport, browser side.
//!
//! Implements the [`Transport`] seam over `web_sys::WebSocket`, moving
//! `webland-protocol` frames as binary messages. Callers turn typed messages
//! into frames with `webland_protocol::{encode, decode}` — the same codec the
//! backend runs.

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, MessageEvent, WebSocket};

use super::Transport;

/// A browser-side WebSocket carrying protocol frames.
#[derive(Debug)]
pub struct WebSocketTransport {
    socket: WebSocket,
}

impl WebSocketTransport {
    /// Open a connection to the backend. Localhost only until the protocol has
    /// authentication (see docs/roadmap.md).
    ///
    /// # Errors
    /// Returns the JS error if the socket cannot be constructed.
    pub fn connect(url: &str) -> Result<Self, wasm_bindgen::JsValue> {
        let socket = WebSocket::new(url)?;
        socket.set_binary_type(BinaryType::Arraybuffer);
        Ok(Self { socket })
    }
}

impl Transport for WebSocketTransport {
    fn send(&self, frame: &[u8]) {
        // A dropped frame surfaces later as a closed socket; nothing to do here.
        let _ = self.socket.send_with_u8_array(frame);
    }

    fn on_message(&self, handler: Box<dyn Fn(Vec<u8>)>) {
        let closure = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
                handler(js_sys::Uint8Array::new(buffer.as_ref()).to_vec());
            }
        });
        self.socket
            .set_onmessage(Some(closure.as_ref().unchecked_ref()));
        // The socket outlives this call; keep the closure alive for its lifetime.
        closure.forget();
    }

    fn close(&self) {
        let _ = self.socket.close();
    }
}

impl WebSocketTransport {
    /// Run `handler` once the socket opens.
    pub fn on_open(&self, handler: Box<dyn Fn()>) {
        let closure = Closure::<dyn FnMut(web_sys::Event)>::new(move |_event| handler());
        self.socket
            .set_onopen(Some(closure.as_ref().unchecked_ref()));
        closure.forget();
    }

    /// Run `handler` when the socket closes or fails to connect.
    pub fn on_close(&self, handler: Box<dyn Fn()>) {
        let closure = Closure::<dyn FnMut(web_sys::Event)>::new(move |_event| handler());
        self.socket
            .set_onclose(Some(closure.as_ref().unchecked_ref()));
        self.socket
            .set_onerror(Some(closure.as_ref().unchecked_ref()));
        closure.forget();
    }
}
