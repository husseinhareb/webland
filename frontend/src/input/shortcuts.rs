//! Keys the shell keeps for itself: Alt+Tab, Alt+1..4, Alt+F4 and Alt+Shift+W.
//!
//! These never reach a client. Everything else does. See [`super::keyboard`].

use leptos::prelude::*;
use web_sys::KeyboardEvent;
use webland_core::SurfaceId;
use webland_protocol::{ClientMessage, Press, encode};

use crate::protocol::WebSocketTransport;
use crate::scene::{AltTabState, Scene};

/// Is this event one of the shell's own chords?
pub fn claims(event: &KeyboardEvent) -> bool {
    event.alt_key()
        && !event.ctrl_key()
        && (matches!(
            event.code().as_str(),
            "Tab" | "Digit1" | "Digit2" | "Digit3" | "Digit4" | "F4"
        ) || (event.shift_key() && event.code() == "KeyW"))
}

/// Act on one. Only the press does anything; the release is swallowed so the
/// client never sees half a chord.
pub fn handle(scene: &Scene, transport: &WebSocketTransport, event: &KeyboardEvent, press: Press) {
    if press != Press::Down {
        return;
    }
    match event.code().as_str() {
        "Tab" => alt_tab(scene, event.shift_key()),
        "Digit1" => switch_workspace(scene, transport, 0),
        "Digit2" => switch_workspace(scene, transport, 1),
        "Digit3" => switch_workspace(scene, transport, 2),
        "Digit4" => switch_workspace(scene, transport, 3),
        _ => {
            if let Some(id) = scene.focused.get_untracked() {
                let _ = encode(&ClientMessage::CloseSurface { id: SurfaceId(id) })
                    .map(|frame| transport.send(&frame));
            }
        }
    }
}

/// Open the switcher, or step the selection along it. Shift walks backwards.
fn alt_tab(scene: &Scene, backwards: bool) {
    if scene.alt_tab.get_untracked().is_some() {
        scene.alt_tab.update(|state| {
            if let Some(s) = state
                && !s.window_ids.is_empty()
            {
                let step = if backwards { s.window_ids.len() - 1 } else { 1 };
                s.selected_index = (s.selected_index + step) % s.window_ids.len();
            }
        });
        return;
    }
    let current = scene.workspace.get_untracked();
    let window_ids = scene.windows.with_untracked(|ws| {
        let mut visible: Vec<_> = ws
            .iter()
            .filter(|w| w.parent.is_none() && w.workspace == current && !w.minimized)
            .collect();
        visible.sort_by_key(|w| std::cmp::Reverse(w.z));
        visible.iter().map(|w| w.id).collect::<Vec<_>>()
    });
    if window_ids.is_empty() {
        return;
    }
    // The one below the top, so a single Alt+Tab swaps the two most recent
    // windows rather than reselecting the one already in front.
    let selected_index = usize::from(window_ids.len() > 1);
    scene.alt_tab.set(Some(AltTabState {
        selected_index,
        window_ids,
    }));
}

/// Commit the switcher's selection, which is what releasing Alt means.
pub fn commit_alt_tab(scene: &Scene, transport: &WebSocketTransport) {
    let mut committed = None;
    scene.alt_tab.update(|s| committed = s.take());
    if let Some(state) = committed
        && let Some(&id) = state.window_ids.get(state.selected_index)
    {
        scene.raise(SurfaceId(id));
        present_and_focus(transport, id);
    }
}

fn switch_workspace(scene: &Scene, transport: &WebSocketTransport, workspace: u32) {
    scene.workspace.set(workspace);
    scene.show_toast(format!("Workspace {}", workspace + 1), None);
    // Whatever is on top there takes the seat; an empty workspace focuses
    // nothing, which is what the compositor is told by saying nothing.
    let next_top = scene.windows.with_untracked(|ws| {
        ws.iter()
            .filter(|w| w.parent.is_none() && w.workspace == workspace && !w.minimized)
            .max_by_key(|w| w.z)
            .map(|w| w.id)
    });
    scene.focused.set(next_top);
    if let Some(id) = next_top
        && let Ok(frame) = encode(&ClientMessage::Focus { id: SurfaceId(id) })
    {
        transport.send(&frame);
    }
}

/// Ack a frame for a window being brought forward and hand it the seat. The ack
/// steps a throttled surface's frame clock straight out of its idle pace, so the
/// window the user just picked is not the one still showing a stale frame.
pub fn present_and_focus(transport: &WebSocketTransport, id: u64) {
    for message in [
        ClientMessage::FramePresented { id: SurfaceId(id) },
        ClientMessage::Focus { id: SurfaceId(id) },
    ] {
        if let Ok(frame) = encode(&message) {
            transport.send(&frame);
        }
    }
}
