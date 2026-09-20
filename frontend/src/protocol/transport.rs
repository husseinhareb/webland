//! WebSocket transport, browser side.
//!
//! Moves `webland-protocol` frames over `web_sys::WebSocket` as binary messages.
//! Callers turn typed messages into frames with `webland_protocol::{encode,
//! decode}` — the same codec the backend runs.
//!
//! Frames are opaque bytes here; WebTransport would be a second type with the
//! same four methods, and nothing below assumes a socket beyond that.

use std::cell::Cell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, MessageEvent, WebSocket};

/// A browser-side WebSocket carrying protocol frames.
#[derive(Debug)]
pub struct WebSocketTransport {
    socket: WebSocket,
    /// Bytes this socket has carried, each way, since it opened. Counted here
    /// because this is the one place every frame passes through, and shown in
    /// the panel: on a link that is not loopback, the bandwidth a desktop costs
    /// is the thing worth watching.
    traffic: Rc<Traffic>,
}

/// Bytes sent and received on one socket.
#[derive(Debug, Default)]
pub struct Traffic {
    sent: Cell<u64>,
    received: Cell<u64>,
}

impl Traffic {
    /// Totals since the socket opened: `(received, sent)`.
    #[must_use]
    pub fn totals(&self) -> (u64, u64) {
        (self.received.get(), self.sent.get())
    }
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
        Ok(Self {
            socket,
            traffic: Rc::new(Traffic::default()),
        })
    }

    /// What this socket has carried, for whoever wants to display it.
    #[must_use]
    pub fn traffic(&self) -> Rc<Traffic> {
        self.traffic.clone()
    }

    /// Put a frame on the wire.
    pub fn send(&self, frame: &[u8]) {
        self.traffic
            .sent
            .set(self.traffic.sent.get() + frame.len() as u64);
        // A dropped frame surfaces later as a closed socket; nothing to do here.
        let _ = self.socket.send_with_u8_array(frame);
    }

    /// Run `handler` for every frame that arrives.
    pub fn on_message(&self, handler: Box<dyn Fn(Vec<u8>)>) {
        let traffic = self.traffic.clone();
        let closure = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
                let frame = js_sys::Uint8Array::new(buffer.as_ref()).to_vec();
                traffic
                    .received
                    .set(traffic.received.get() + frame.len() as u64);
                handler(frame);
            }
        });
        self.socket
            .set_onmessage(Some(closure.as_ref().unchecked_ref()));
        // The socket outlives this call; keep the closure alive for its lifetime.
        closure.forget();
    }

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
