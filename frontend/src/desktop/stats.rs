//! What the desktop is costing on the wire, at the end of the panel.
//!
//! Every window is a video stream, so bandwidth is the number that decides
//! whether Webland is usable over a given link — and the only way to find out
//! today is a terminal on the other machine. A rate in the panel makes the cost
//! of a habit (a full-screen video, four windows animating at once) visible
//! while it is being formed.
//!
//! Down is frames arriving, up is input and acks going back, both measured on
//! the socket itself rather than estimated from anything.

use std::rc::Rc;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::protocol::WebSocketTransport;

/// How often the rate is recomputed. A second: short enough to answer "what did
/// that just cost", long enough that the number can be read.
const INTERVAL_MS: i32 = 1000;

#[component]
pub fn Stats(
    transport: StoredValue<Option<Rc<WebSocketTransport>>, LocalStorage>,
) -> impl IntoView {
    let rates = RwSignal::new((0.0, 0.0));
    let traffic = transport.with_value(|t| t.as_ref().map(|t| t.traffic()))?;

    let mut last = traffic.totals();
    let tick = Closure::<dyn FnMut()>::new(move || {
        let now = traffic.totals();
        let seconds = f64::from(INTERVAL_MS) / 1000.0;
        // Saturating, because the totals are `u64`: subtracting them the wrong
        // way round is not a small negative number, it is an enormous positive
        // one, and the panel would report gigabytes a second.
        rates.set((
            bytes_per_second(now.0.saturating_sub(last.0), seconds),
            bytes_per_second(now.1.saturating_sub(last.1), seconds),
        ));
        last = now;
    });
    if let Some(window) = web_sys::window() {
        let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(
            tick.as_ref().unchecked_ref(),
            INTERVAL_MS,
        );
    }
    tick.forget();

    Some(view! {
        <span class="stats" title="Protocol traffic: down is frames, up is input">
            {move || {
                let (down, up) = rates.get();
                format!("↓ {} ↑ {}", rate(down), rate(up))
            }}
        </span>
    })
}

/// Bytes per second, at the scale a person reads.
fn rate(bytes: f64) -> String {
    if bytes >= 1_000_000.0 {
        format!("{:.1} MB/s", bytes / 1_000_000.0)
    } else if bytes >= 1_000.0 {
        format!("{:.0} kB/s", bytes / 1_000.0)
    } else {
        format!("{bytes:.0} B/s")
    }
}

/// A byte count over a window, as a rate. Precision loss at these magnitudes is
/// a fraction of a byte per second.
#[allow(clippy::cast_precision_loss)]
fn bytes_per_second(bytes: u64, seconds: f64) -> f64 {
    bytes as f64 / seconds
}
