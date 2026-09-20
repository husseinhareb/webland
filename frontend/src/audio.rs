//! The session's sound, played in the page.
//!
//! The compositor streams its own audio sink as WebM/Opus (see the server's
//! `audio` module), and this is the other end: a `MediaSource` fed by the
//! protocol, playing through an `<audio>` element that never appears on the
//! page. Nothing here decodes anything; the browser does, from the stream's
//! own header, which is why the bytes are opaque all the way through the
//! protocol.
//!
//! Two things make a socket-fed `MediaSource` different from a file:
//!
//! - **Autoplay.** A page that has not been interacted with may not make sound,
//!   and the refusal arrives as a rejected promise rather than an error. So the
//!   first pointer or key event the page sees tries again.
//! - **Drift.** Chunks arrive in real time and the element plays them at its own
//!   rate, so the buffer grows and playback falls steadily further behind what
//!   is happening on screen. Whenever it is more than [`MAX_LAG`] behind, it
//!   skips to the live edge.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{HtmlAudioElement, MediaSource, SourceBuffer};

/// What the server sends, and the only thing this asks the browser to play.
const MIME: &str = "audio/webm; codecs=opus";

/// How far behind the live edge playback is allowed to fall before it skips.
/// Generous enough to absorb a hiccup, short enough that sound stays with the
/// picture.
const MAX_LAG: f64 = 0.5;

/// A player for the session's audio stream.
pub struct Player {
    /// Never added to the document: it exists to play, not to be seen. Held
    /// here so it and its media source are not collected.
    element: HtmlAudioElement,
    source: MediaSource,
    buffer: Rc<RefCell<Option<SourceBuffer>>>,
    /// Chunks that arrived while the source buffer was busy with the last one.
    /// `appendBuffer` is asynchronous and throws if called again before it
    /// finishes, so everything queues here and drains on `updateend`.
    pending: Rc<RefCell<VecDeque<Vec<u8>>>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

impl Player {
    /// Build a player. `None` if the browser has no `MediaSource` or will not
    /// play Opus in `WebM`, in which case the desktop is silent and everything
    /// else works.
    #[must_use]
    pub fn new() -> Option<Self> {
        if !MediaSource::is_type_supported(MIME) {
            return None;
        }
        let source = MediaSource::new().ok()?;
        let element = HtmlAudioElement::new().ok()?;
        element.set_autoplay(true);
        let url = web_sys::Url::create_object_url_with_source(&source).ok()?;
        element.set_src(&url);

        let buffer: Rc<RefCell<Option<SourceBuffer>>> = Rc::new(RefCell::new(None));
        let pending: Rc<RefCell<VecDeque<Vec<u8>>>> = Rc::new(RefCell::new(VecDeque::new()));

        // The source buffer cannot be added until the media source is open,
        // and anything that arrives before then waits in `pending`.
        let ready = source.clone();
        let opened_buffer = buffer.clone();
        let opened_pending = pending.clone();
        let opened = Closure::<dyn FnMut()>::new(move || {
            let Ok(source_buffer) = ready.add_source_buffer(MIME) else {
                return;
            };
            let drain_buffer = opened_buffer.clone();
            let drain_pending = opened_pending.clone();
            let drained = Closure::<dyn FnMut()>::new(move || {
                append_next(&drain_buffer, &drain_pending);
            });
            source_buffer.set_onupdateend(Some(drained.as_ref().unchecked_ref()));
            drained.forget();
            *opened_buffer.borrow_mut() = Some(source_buffer);
            append_next(&opened_buffer, &opened_pending);
        });
        source.set_onsourceopen(Some(opened.as_ref().unchecked_ref()));
        opened.forget();

        let player = Self {
            element,
            source,
            buffer,
            pending,
        };
        player.play();
        player.play_on_first_gesture();
        Some(player)
    }

    /// Take one chunk of the stream.
    pub fn push(&self, chunk: Vec<u8>) {
        self.pending.borrow_mut().push_back(chunk);
        append_next(&self.buffer, &self.pending);
        self.catch_up();
    }

    /// Ask the element to play, and say nothing when the browser refuses:
    /// a page with no interaction yet is not allowed to, and
    /// [`Self::play_on_first_gesture`] is what handles that.
    fn play(&self) {
        let _ = self.element.play();
    }

    /// Try again the first time the user touches the page, which is the moment
    /// the browser starts allowing sound.
    fn play_on_first_gesture(&self) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let element = self.element.clone();
        let listener = Closure::<dyn FnMut()>::new(move || {
            let _ = element.play();
        });
        for event in ["pointerdown", "keydown"] {
            let _ = window.add_event_listener_with_callback_and_bool(
                event,
                listener.as_ref().unchecked_ref(),
                true,
            );
        }
        listener.forget();
    }

    /// Skip to the live edge when playback has fallen behind it.
    ///
    /// The stream arrives in real time, so anything buffered is time the sound
    /// is behind the picture. Seeking is audible, which is why it happens at
    /// half a second rather than at the first sample.
    fn catch_up(&self) {
        let Ok(buffered) = self.element.buffered().end(0) else {
            return;
        };
        if buffered - self.element.current_time() > MAX_LAG {
            self.element.set_current_time(buffered - 0.1);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.source.end_of_stream();
    }
}

/// Hand the source buffer the next chunk, if it is ready for one.
fn append_next(
    buffer: &Rc<RefCell<Option<SourceBuffer>>>,
    pending: &Rc<RefCell<VecDeque<Vec<u8>>>>,
) {
    let held = buffer.borrow();
    let Some(source_buffer) = held.as_ref() else {
        return;
    };
    if source_buffer.updating() {
        return;
    }
    let Some(mut chunk) = pending.borrow_mut().pop_front() else {
        return;
    };
    // A failed append is a stream the browser cannot make sense of any more;
    // the next one that arrives is as good a place to recover as any.
    let _ = source_buffer.append_buffer_with_u8_array(&mut chunk);
}
