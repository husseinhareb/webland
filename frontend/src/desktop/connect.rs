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
use crate::scene::{Scene, Settings, pixel_ratio};

use super::shell::window_size;

/// Path the protocol socket is served on, proxied to the backend by whatever is
/// serving the page (see `Trunk.toml`).
const BACKEND_PATH: &str = "/ws";

/// Also what a refused token looks like: the browser reports a failed upgrade
/// as a close, with no status to tell the two apart.
const DISCONNECTED: &str =
    "disconnected; is the backend running? A first visit needs the #token= link it logs.";

/// The cookie the backend checks every request for (its `auth` module).
const TOKEN_COOKIE: &str = "webland_token";

/// Take the session token from `#token=` in the page's URL, if it is there.
///
/// Kept as a cookie because a cookie is the one credential the browser attaches
/// to everything by itself: the socket upgrade, `fetch`, and the wallpaper as
/// an image. `SameSite=Strict` stops another site from having it attached on
/// its behalf. The fragment is then wiped from the address bar, so the token is
/// not left on screen.
pub fn adopt_token() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let location = window.location();
    let Some(token) = location
        .hash()
        .ok()
        .and_then(|hash| hash.strip_prefix("#token=").map(str::to_owned))
    else {
        return;
    };
    // The server makes hex; anything else is not a token, and could smuggle
    // attributes into the cookie.
    if token.is_empty() || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return;
    }
    let secure = if location.protocol().is_ok_and(|scheme| scheme == "https:") {
        "; Secure"
    } else {
        ""
    };
    if let Some(document) = window
        .document()
        .and_then(|document| document.dyn_into::<web_sys::HtmlDocument>().ok())
    {
        let _ = document.set_cookie(&format!(
            "{TOKEN_COOKIE}={token}; Path=/; Max-Age=31536000; SameSite=Strict{secure}"
        ));
    }
    let clean = format!(
        "{}{}",
        location.pathname().unwrap_or_default(),
        location.search().unwrap_or_default()
    );
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&clean));
    }
}

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
    let settings = scene.settings;
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
            && let Ok(frame) = encode(&ClientMessage::Resize {
                size,
                scale: pixel_ratio(),
            })
        {
            opened.send(&frame);
        }
        if let Ok(frame) = encode(&ClientMessage::RequestKeyframe) {
            opened.send(&frame);
        }
        // And that this keyboard holds nothing, which the seat cannot know:
        // it keeps whatever the last page left pressed.
        crate::input::release_modifiers(&opened);
        // And which keyboard it is, if the viewer said.
        send_keymap(&opened, settings, false);
        measure_refresh(opened.clone(), Vec::new());
    }));
    follow_keymap(transport.clone(), settings);
    follow_window(transport.clone());
    transport.on_close(Box::new(move || {
        status.set(String::from(DISCONNECTED));
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
        // A *new* window takes the seat, and only a new one: `SurfaceCreated` is
        // re-sent for a surface that merely resized, and asking for the focus
        // again on one of those is read by the compositor as a click outside
        // whatever menu is open, which took the menu down. Firefox resizes its
        // window as it opens a context menu, so its own menu died the moment it
        // appeared. Read before `scene.handle`, while the scene still holds what
        // it knew before this message.
        if let ServerMessage::SurfaceCreated(ref created) = message
            && created.parent.is_none()
            && !scene.knows(created.id)
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

/// Follow the window: the browser is the display, so its size is the screen
/// resolution and a resized window is a mode change.
fn follow_window(transport: Rc<WebSocketTransport>) {
    let listener = Closure::<dyn FnMut()>::new(move || {
        if let Some(size) = window_size()
            && let Ok(frame) = encode(&ClientMessage::Resize {
                size,
                scale: pixel_ratio(),
            })
        {
            transport.send(&frame);
        }
    });
    if let Some(window) = web_sys::window() {
        let _ =
            window.add_event_listener_with_callback("resize", listener.as_ref().unchecked_ref());
    }
    listener.forget();
}

/// A layout picked in the settings applies at once. The first run is the
/// stored one, which the connection sends itself once it opens.
fn follow_keymap(transport: Rc<WebSocketTransport>, settings: Settings) {
    Effect::new(move |first: Option<()>| {
        settings.layout.track();
        settings.variant.track();
        if first.is_some() {
            send_keymap(&transport, settings, true);
        }
    });
}

/// Animation frames timed to measure the display's refresh: two seconds at
/// 60 Hz.
const REFRESH_SAMPLES: usize = 120;

/// Time animation frames and tell the compositor the display's refresh, which
/// it paces frame callbacks to.
///
/// A hidden page gets no animation frames, so a tab opened in the background
/// measures once it is first shown, and the compositor assumes 60 Hz until
/// then.
fn measure_refresh(transport: Rc<WebSocketTransport>, mut stamps: Vec<f64>) {
    request_animation_frame(move || {
        if let Some(now) = web_sys::window()
            .and_then(|window| window.performance())
            .map(|performance| performance.now())
        {
            stamps.push(now);
        }
        if stamps.len() < REFRESH_SAMPLES {
            return measure_refresh(transport, stamps);
        }
        let deltas: Vec<f64> = stamps.windows(2).map(|pair| pair[1] - pair[0]).collect();
        let mut sorted = deltas.clone();
        sorted.sort_by(f64::total_cmp);
        let typical = sorted[sorted.len() / 2];
        // The mean of the frames near the median: the median alone carries the
        // callback's jitter, and a frame the page was too busy to take is a
        // skipped refresh, not a slow screen.
        let near: Vec<f64> = deltas
            .into_iter()
            .filter(|delta| (delta - typical).abs() < typical * 0.25)
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let interval = near.iter().sum::<f64>() / near.len() as f64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let millihertz = (1_000_000.0 / interval).round() as u32;
        if interval > 0.0
            && let Ok(frame) = encode(&ClientMessage::RefreshRate { millihertz })
        {
            transport.send(&frame);
        }
    });
}

/// Tell the compositor which xkb layout to read keys with. An empty one is
/// only worth sending as a change back to the host's.
fn send_keymap(transport: &WebSocketTransport, settings: Settings, changed: bool) {
    if !changed && settings.layout.with_untracked(String::is_empty) {
        return;
    }
    if let Ok(frame) = encode(&ClientMessage::SetKeymap {
        layout: settings.layout.get_untracked().trim().to_owned(),
        variant: settings.variant.get_untracked().trim().to_owned(),
    }) {
        transport.send(&frame);
    }
}
