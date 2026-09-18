//! Where the listeners are attached, and the one helper they all share.

use std::rc::Rc;

use web_sys::Element;
use webland_protocol::{ClientMessage, InputEvent, encode};

use crate::latency::Latency;
use crate::protocol::WebSocketTransport;
use crate::scene::Scene;

/// Attach pointer, wheel and keyboard listeners that stream input to the
/// compositor.
pub fn wire(
    container: &Element,
    scene: &Scene,
    transport: &Rc<WebSocketTransport>,
    latency: &Rc<Latency>,
) {
    super::pointer::install(container, scene, transport, latency);
    super::wheel::install(scene, transport);
    super::keyboard::install(scene, transport, latency);
}

pub fn send(transport: &WebSocketTransport, event: InputEvent) {
    if let Ok(bytes) = encode(&ClientMessage::Input(event)) {
        transport.send(&bytes);
    }
}
