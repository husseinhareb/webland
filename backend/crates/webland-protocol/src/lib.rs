//! Wire format for the Webland protocol.
//!
//! The message set is specified in `shared/protocol/` and implemented once
//! here. Because the frontend is Rust, it depends on this crate directly, so
//! there is a single codec and the two ends cannot drift.
//!
//! Transport is deliberately left out: the first transport is WebSocket, and
//! nothing here may assume it, so WebTransport can be dropped in later. The
//! binary encoding is likewise undecided; these types fix the *shape* of the
//! three messages, not their bytes.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use webland_core::{Point, Rect, Size, SurfaceId};

/// Protocol version negotiated at connect time.
pub const VERSION: u32 = 0;

/// A framing error. The wire encoding is `bincode`; a transport delivers whole
/// messages, so a frame is one encoded [`ServerMessage`] or [`ClientMessage`].
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// A message could not be encoded.
    #[error("encode: {0}")]
    Encode(String),
    /// A frame could not be decoded into the expected message.
    #[error("decode: {0}")]
    Decode(String),
}

/// Encode a message into a single wire frame.
///
/// # Errors
/// Returns [`WireError::Encode`] if serialization fails.
pub fn encode<M: Serialize>(message: &M) -> Result<Vec<u8>, WireError> {
    bincode::serialize(message).map_err(|e| WireError::Encode(e.to_string()))
}

/// Decode a single wire frame into a message.
///
/// # Errors
/// Returns [`WireError::Decode`] if the bytes are not a valid `M`.
pub fn decode<M: DeserializeOwned>(bytes: &[u8]) -> Result<M, WireError> {
    bincode::deserialize(bytes).map_err(|e| WireError::Decode(e.to_string()))
}

/// Compress a raw pixel buffer for a [`Codec::Deflate`] frame. Level 1: fast,
/// and repetitive UI/terminal pixels still shrink enormously.
#[must_use]
pub fn deflate(bytes: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec(bytes, 1)
}

/// Decompress a [`Codec::Deflate`] payload back to raw pixels.
///
/// # Errors
/// Returns [`WireError::Decode`] if the data is not valid deflate.
pub fn inflate(bytes: &[u8]) -> Result<Vec<u8>, WireError> {
    miniz_oxide::inflate::decompress_to_vec(bytes)
        .map_err(|err| WireError::Decode(format!("inflate: {err:?}")))
}

/// How a surface frame's pixels are encoded in [`SurfaceFrame::payload`].
///
/// A terminal is just a highly compressible video: every surface travels this
/// path, `Raw` for the `wl_shm` easy case and a real codec for dmabuf clients
/// encoded on the GPU (see Decision 2 in the roadmap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    /// Uncompressed pixels. The `wl_shm` path, and the simplest to bring up.
    Raw,
    /// Raw pixels, deflate-compressed (see [`deflate`]/[`inflate`]). A CPU-side
    /// stopgap that makes the `wl_shm` path usable before GPU video encode.
    Deflate,
    /// H.264 bitstream, VA-API encoded directly from a client dmabuf.
    H264,
}

/// An application the compositor is willing to start.
///
/// The browser picks one by `id` and never sends a command line. That is the
/// whole point of naming them: the compositor runs only what it found itself, so
/// a launcher cannot become a way to run arbitrary programs on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Application {
    pub id: u32,
    pub name: String,
    /// The `.desktop` file's basename (`firefox`, `org.gnome.Nautilus`), which
    /// is also what a Wayland client reports as its `app_id`. It is the only
    /// thing the two sides have in common, and so the only way the panel can
    /// put an application's icon on its window's task button.
    pub app_id: String,
    /// The application's icon as a `data:` URL, when one was found; the
    /// launcher is a list a person reads, and it reads faster with pictures.
    pub icon: Option<String>,
}

/// One icon in the system tray, as its application describes itself.
///
/// The `id` is the item's address on the session bus and the handle for
/// everything the browser can do with it. It is opaque on this side of the
/// wire: the browser sends it back, it never parses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayItem {
    pub id: String,
    /// What the item calls itself, for a tooltip. Often empty.
    pub title: String,
    /// The icon as a `data:` URL; the item's own pixmap re-encoded, or the
    /// file its icon name resolved to in the icon theme.
    pub icon: Option<String>,
}

/// One row of a tray item's menu.
///
/// Flattened from `com.canonical.dbusmenu`, which is a tree of properties the
/// browser has no business knowing about. What survives is what a menu is: a
/// label, whether it can be clicked, whether it is ticked, and its children.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayMenuItem {
    /// The dbusmenu id, sent back to say which row was clicked.
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    /// `Some` for a checkbox or radio row, and then whether it is ticked.
    pub checked: Option<bool>,
    /// A rule rather than a row: no label, nothing to click.
    pub separator: bool,
    pub children: Vec<TrayMenuItem>,
}

/// Where a popup hangs: which surface it belongs to, and where on it.
///
/// A menu is not a window. It has no chrome, no place in the panel and no
/// position of its own; the client decided where it goes relative to the
/// surface that opened it, and the browser's only job is to put it there and
/// keep it there while that surface moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub parent: SurfaceId,
    /// Offset from the parent window's top-left, in the same pixels as `size`.
    pub x: i32,
    pub y: i32,
}

/// A surface appeared; the browser should allocate a scene node for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceCreated {
    pub id: SurfaceId,
    pub size: Size,
    /// The window itself within that image, in the same pixels.
    ///
    /// A client that draws its own shadow commits a buffer bigger than its
    /// window and says so in its geometry; the margin is transparent to the
    /// client and black once encoded, so the browser is told what to show and
    /// clips the rest away.
    pub content: Rect,
    /// Set when this surface is a popup (a menu, a tooltip, a combobox list)
    /// rather than a window of its own.
    pub parent: Option<Anchor>,
    /// Whether the shell should draw this window's chrome.
    ///
    /// False for a client that decorates itself: a GTK application, whose
    /// headerbar is part of the window it drew. Such a client never asks for a
    /// decoration mode, because it does not implement the protocol that would
    /// let the compositor answer, so the shell drawing a titlebar of its own
    /// would put a second one directly above the client's.
    pub decorated: bool,
}

/// A window-management gesture that began inside the client, not the shell.
///
/// A self-decorating client's own titlebar is where these come from: dragging it
/// is `Move`, double-clicking it is `Maximize`, and its buttons are the rest.
/// The shell owns window position and stacking, so the client can only ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowRequest {
    /// Follow the pointer until the button comes up, as a titlebar drag does.
    Move,
    Maximize,
    Unmaximize,
    Minimize,
}

/// New contents for a surface.
///
/// `damage` bounds the changed region so an idle surface costs nothing;
/// `payload` is `codec`-encoded, its byte layout fixed by the encoding chosen
/// later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceFrame {
    pub id: SurfaceId,
    pub codec: Codec,
    pub damage: Vec<Rect>,
    pub payload: Vec<u8>,
}

/// Pressed or released, shared by pointer buttons and keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Press {
    Down,
    Up,
}

/// Input originating in the browser, on its way to a Wayland client.
///
/// The keyboard shape is intentionally minimal: xkb keymaps, key repeat and IME
/// are Phase 3 problems, not Phase 0 ones. `keycode` is a raw evdev code, the
/// unit the compositor ultimately needs.
///
/// Motion and scroll name the surface they landed on, and nothing else does.
/// That is the split Wayland already makes: the pointer goes where it is
/// pointed, and the keyboard goes where the focus is, while buttons and wheel
/// notches follow the pointer's own focus, which the surface below establishes.
/// Without the id every one of these went to the focused window, so hovering or
/// scrolling an unfocused one moved the pointer inside the focused one instead.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    /// `position` is in the named surface's own pixels.
    PointerMotion {
        id: SurfaceId,
        position: Point,
    },
    PointerMotionRelative {
        dx: f64,
        dy: f64,
    },
    PointerButton {
        button: u32,
        state: Press,
    },
    PointerScroll {
        id: SurfaceId,
        dx: f64,
        dy: f64,
    },
    Key {
        keycode: u32,
        state: Press,
    },
}

/// Backend → browser. One of the two server-originated messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    SurfaceCreated(SurfaceCreated),
    SurfaceFrame(SurfaceFrame),
    /// What the launcher may start, sent when a browser connects.
    Applications(Vec<Application>),
    /// The surface's title changed, or was seen for the first time.
    ///
    /// Separate from `SurfaceCreated` because a client sets its title whenever
    /// it likes (a terminal rewrites it on every command) and the browser
    /// wants the new one without a new surface.
    SurfaceTitle {
        id: SurfaceId,
        title: String,
    },
    /// Which application the surface belongs to, as the client names itself.
    ///
    /// Its own message for the same reason as the title: a client may set it
    /// after mapping, and an X11 client's class arrives whenever its window
    /// manager gets round to reading it.
    SurfaceAppId {
        id: SurfaceId,
        app_id: String,
    },
    /// The surface's client asked the shell to move, maximize or minimize it.
    ///
    /// Only self-decorating clients send these, and only because their own
    /// titlebar is the one the user grabbed. A client that lets the compositor
    /// decorate never asks: the shell's own chrome is already the one being
    /// clicked, and it acts without a round trip.
    SurfaceRequest {
        id: SurfaceId,
        request: WindowRequest,
    },
    /// The system tray's contents, whenever they change.
    ///
    /// Whole list rather than a delta: a tray holds a handful of icons, and a
    /// browser that just connected needs the whole thing anyway.
    Tray {
        items: Vec<TrayItem>,
    },
    /// The menu of one tray item, in answer to [`ClientMessage::TrayMenuOpen`].
    ///
    /// Fetched when it is asked for, never cached: a tray menu says what an
    /// application is doing right now (connected networks, playing or paused)
    /// and a stale one is worse than a slow one.
    TrayMenu {
        id: String,
        items: Vec<TrayMenuItem>,
    },
    /// A piece of the session's audio, as a WebM/Opus byte stream.
    ///
    /// Opaque and ordered: the browser appends these to a media source in the
    /// order they arrive and nothing here looks inside them. The first chunk a
    /// connection receives is the stream's header, so the capture is started
    /// per browser rather than shared; a browser that joined halfway through
    /// somebody else's stream would have nothing to initialise a decoder with.
    Audio {
        payload: Vec<u8>,
    },
    /// A client put this text on the clipboard; the browser should too, so a
    /// copy inside webland can be pasted anywhere on the machine.
    Clipboard {
        text: String,
    },
    /// A surface has requested or released a pointer lock constraint (e.g. Minecraft in-game).
    PointerConstraint {
        id: SurfaceId,
        locked: bool,
    },
    /// What the pointer should look like over a client's surface.
    ///
    /// The name is a CSS cursor keyword, which is also the XDG cursor name the
    /// client asked for; the two vocabularies are the same one, so the browser
    /// can hand it straight to the stylesheet. `none` hides the pointer, which
    /// is what a client that draws its own does.
    ///
    /// Sent for the seat, not per surface: there is one pointer, and only the
    /// surface it is over has any say in how it looks.
    Cursor {
        name: String,
    },
    /// The surface is gone; the browser should drop its scene node.
    ///
    /// Without this a closed window stays on screen forever: the browser has no
    /// other way to tell "this client exited" from "this surface is idle", and
    /// idle surfaces are supposed to cost nothing.
    SurfaceDestroyed {
        id: SurfaceId,
    },
}

/// Browser → backend. Input plus the frame-pacing ack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Input on its way to a Wayland client.
    Input(InputEvent),
    /// The browser has presented a frame of this surface and is ready for the
    /// next one. This is what lets the browser drive the frame clock
    /// (Decision 3): the server holds back until it arrives, so the in-flight
    /// queue stays bounded.
    ///
    /// The surface is named because credit is per surface. A global clock would
    /// let one busy window spend the callbacks owed to every other one, so three
    /// applications would pace each other rather than each pacing itself.
    FramePresented { id: SurfaceId },
    /// The browser raised this surface; send input there from now on.
    ///
    /// Stacking is browser-side state the compositor is never told about. Focus
    /// is the one part it must know, because there is a single seat and somebody
    /// has to receive the keystrokes.
    Focus { id: SurfaceId },
    /// The browser's clipboard, as of the paste the user just asked for.
    ///
    /// Sent on the paste rather than whenever the clipboard changes, because a
    /// page cannot read a clipboard it was not handed: the paste event is the
    /// browser handing it over, and needs no permission to do it.
    Clipboard { text: String },
    /// Start the application with this id, as the launcher does.
    Launch { id: u32 },
    /// Click a tray icon: the item's own action, or its alternate one.
    ///
    /// What the action does is entirely the application's business; most
    /// present a window, some toggle something, some only have a menu and do
    /// nothing at all here.
    TrayActivate { id: String, secondary: bool },
    /// Ask for a tray item's menu, answered by [`ServerMessage::TrayMenu`].
    TrayMenuOpen { id: String },
    /// Pick a row of the menu last opened for this item.
    TrayMenuClick { id: String, item: i32 },
    /// Ask the surface's client to close, as a window button does.
    ///
    /// A request, not an order: the client may put up a save dialog, or ignore
    /// it. The surface goes away when the client says so, via `SurfaceDestroyed`.
    CloseSurface { id: SurfaceId },
    /// Maximize this surface to `size`, or restore it when `size` is `None`.
    ///
    /// Maximizing is the one window-management gesture the compositor has to
    /// hear about, because only the client can act on it: a shell that merely
    /// stretched the window would be scaling a smaller surface over a bigger
    /// box. The browser sends the size because the browser is the display and
    /// knows what is left over once its own panel has taken its strip.
    ///
    /// Minimizing sends nothing: a hidden window is browser-side state, exactly
    /// as moving and restacking are.
    SetMaximized { id: SurfaceId, size: Option<Size> },
    /// Configure this surface at `size`, as a resize grip does.
    ///
    /// Same reason as `SetMaximized`: only the client can redraw at a new size,
    /// and a shell that stretched the box instead would be scaling a surface
    /// rendered for a smaller one. Sent when the gesture ends rather than
    /// throughout it, every configure costs the client a reallocation and the
    /// wire a keyframe, so a drag would spend hundreds for one useful answer.
    SetSize { id: SurfaceId, size: Size },
    /// The size the browser wants surfaces configured at, in device pixels.
    ///
    /// Headless has no output, so without this the compositor invents a size
    /// from an environment variable and every client renders at it regardless of
    /// the window it is actually displayed in; too small, and then upscaled by
    /// the browser, which is what makes it look soft. The browser is the display
    /// here, so the browser is what knows the answer.
    Resize { size: Size },
    /// Send full contents for every surface on the next frame.
    ///
    /// Frames carry only damaged regions, so a browser joining mid-stream has
    /// nothing to apply them to. It asks once on connect; without this the
    /// server would have to spend a full surface periodically on the chance
    /// that someone is listening.
    RequestKeyframe,
}

#[cfg(test)]
mod tests {
    use super::{
        ClientMessage, Codec, InputEvent, ServerMessage, SurfaceCreated, SurfaceFrame, decode,
        encode,
    };
    use webland_core::{Point, Rect, Size, SurfaceId};

    // Round-trips through the real wire codec (`encode`/`decode`); the exact
    // bytes both the backend and the WASM frontend put on the wire.

    #[test]
    fn surface_created_round_trips() {
        let msg = ServerMessage::SurfaceCreated(SurfaceCreated {
            id: SurfaceId(1),
            size: Size {
                width: 800,
                height: 600,
            },
            content: Rect {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
            parent: None,
            decorated: true,
        });
        let frame = encode(&msg).unwrap();
        assert_eq!(msg, decode::<ServerMessage>(&frame).unwrap());
    }

    #[test]
    fn surface_frame_round_trips() {
        let msg = ServerMessage::SurfaceFrame(SurfaceFrame {
            id: SurfaceId(2),
            codec: Codec::H264,
            damage: vec![Rect {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            }],
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        });
        let frame = encode(&msg).unwrap();
        assert_eq!(msg, decode::<ServerMessage>(&frame).unwrap());
    }

    #[test]
    fn client_input_round_trips() {
        let msg = ClientMessage::Input(InputEvent::PointerMotion {
            id: SurfaceId(3),
            position: Point { x: 12.0, y: 34.0 },
        });
        let frame = encode(&msg).unwrap();
        assert_eq!(msg, decode::<ClientMessage>(&frame).unwrap());

        let rel = ClientMessage::Input(InputEvent::PointerMotionRelative {
            dx: -5.5,
            dy: 10.25,
        });
        let frame = encode(&rel).unwrap();
        assert_eq!(rel, decode::<ClientMessage>(&frame).unwrap());

        let constraint = ServerMessage::PointerConstraint {
            id: SurfaceId(42),
            locked: true,
        };
        let frame = encode(&constraint).unwrap();
        assert_eq!(constraint, decode::<ServerMessage>(&frame).unwrap());
    }

    /// The resize grip's message carries a size, and a variant added after
    /// `SetMaximized` must not be decoded as it: both name a surface and a size,
    /// so a mis-tagged one would silently maximize a window being resized.
    #[test]
    fn set_size_round_trips_and_is_not_set_maximized() {
        let size = Size {
            width: 800,
            height: 600,
        };
        let msg = ClientMessage::SetSize {
            id: SurfaceId(7),
            size,
        };
        let frame = encode(&msg).unwrap();
        assert_eq!(msg, decode::<ClientMessage>(&frame).unwrap());
        let maximized = ClientMessage::SetMaximized {
            id: SurfaceId(7),
            size: Some(size),
        };
        assert_ne!(frame, encode(&maximized).unwrap());
    }

    /// Pointer motion and scroll carry the surface they landed on, so the
    /// compositor can deliver them to the window under the pointer rather than
    /// to whichever one holds the keyboard.
    #[test]
    fn pointer_events_name_their_surface() {
        for msg in [
            ClientMessage::Input(InputEvent::PointerMotion {
                id: SurfaceId(9),
                position: Point { x: 1.0, y: 2.0 },
            }),
            ClientMessage::Input(InputEvent::PointerScroll {
                id: SurfaceId(9),
                dx: 0.0,
                dy: -120.0,
            }),
        ] {
            assert_eq!(
                msg,
                decode::<ClientMessage>(&encode(&msg).unwrap()).unwrap()
            );
        }
        // Two windows, one gesture: the ids have to survive the wire, or the
        // routing they exist for reads the same for both.
        let a = ClientMessage::Input(InputEvent::PointerScroll {
            id: SurfaceId(1),
            dx: 0.0,
            dy: 8.0,
        });
        let b = ClientMessage::Input(InputEvent::PointerScroll {
            id: SurfaceId(2),
            dx: 0.0,
            dy: 8.0,
        });
        assert_ne!(encode(&a).unwrap(), encode(&b).unwrap());
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode::<ClientMessage>(&[0xff, 0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn deflate_round_trips_and_shrinks() {
        use super::{deflate, inflate};
        let pixels = vec![0x20u8; 64 * 64 * 4]; // a solid surface, as UIs often are
        let packed = deflate(&pixels);
        assert!(packed.len() < pixels.len());
        assert_eq!(inflate(&packed).unwrap(), pixels);
    }
}
