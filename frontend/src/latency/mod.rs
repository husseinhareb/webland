//! Click-to-photon, which is the number Phase 3 is judged on.
//!
//! Both ends of the measurement happen in this process — the input is dispatched
//! here and the frame it causes is drawn here — so `performance.now()` is
//! already the shared clock the roadmap asks for, and nothing has to be
//! synchronised with the compositor.
//!
//! Attribution is by adjacency: an idle Webland sends no frames at all, so the
//! first frame drawn after an input is the frame that input caused. That breaks
//! down under a continuously animating client, where some frame would have
//! arrived anyway and the number comes out flattering. It is honest for the
//! thing Phase 3 cares about — typing, clicking, dragging — and dishonest for
//! video, so read it as interaction latency and not as a frame time.

use std::cell::{Cell, RefCell};

/// How many samples the summary is computed over.
const WINDOW: usize = 60;

/// Input-to-frame timing over a rolling window.
#[derive(Debug, Default)]
pub struct Latency {
    /// When the outstanding input was sent, if one has not yet been answered.
    pending: Cell<Option<f64>>,
    samples: RefCell<Vec<f64>>,
}

impl Latency {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An input just went to the compositor.
    ///
    /// Only the first of a burst is kept: holding a key down produces events
    /// faster than frames come back, and timing the newest against the next
    /// frame would measure the tail of the burst rather than the round trip.
    pub fn input_sent(&self) {
        if self.pending.get().is_none() {
            self.pending.set(now());
        }
    }

    /// A frame reached the screen. Returns the latency it closed, if any.
    pub fn frame_drawn(&self) -> Option<f64> {
        let sent = self.pending.take()?;
        let elapsed = now()? - sent;
        // A negative or absurd sample means the clock or the pairing is wrong;
        // recording it would quietly poison the median.
        if !(0.0..10_000.0).contains(&elapsed) {
            return None;
        }
        let mut samples = self.samples.borrow_mut();
        samples.push(elapsed);
        if samples.len() > WINDOW {
            samples.remove(0);
        }
        Some(elapsed)
    }

    /// Median and worst of the recent samples, for display.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        let samples = self.samples.borrow();
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.clone();
        sorted.sort_by(f64::total_cmp);
        let median = sorted[sorted.len() / 2];
        let worst = *sorted.last()?;
        Some(format!(
            "click-to-photon {median:.0} ms median, {worst:.0} ms worst ({} samples)",
            sorted.len()
        ))
    }
}

/// Milliseconds on the browser's monotonic clock.
fn now() -> Option<f64> {
    Some(web_sys::window()?.performance()?.now())
}
