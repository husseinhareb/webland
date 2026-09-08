//! WebSocket transport: the seam between the compositor and the browser.
//!
//! Frames are `webland-protocol` messages, `bincode`-encoded by the shared
//! codec — the exact same `encode`/`decode` the WASM frontend runs. WebSocket
//! is the first transport; nothing here leaks into the message set, so
//! WebTransport can replace it later.
//!
//! Bound to `127.0.0.1` only: from the moment this works it is an
//! unauthenticated remote desktop, so it stays on loopback until auth exists.

#![allow(clippy::missing_errors_doc)]

use std::collections::HashMap;
use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use webland_protocol::{ClientMessage, ServerMessage, decode, encode};

/// How many frames may be in flight to a browser before it must ack. Small, so
/// latency stays low; >1 so the pipeline does not stall on a single round trip.
const INITIAL_CREDIT: i32 = 2;

/// Per-connection frame pacing (Decision 3), per surface.
///
/// The browser acks every presented frame; the server sends only while it has
/// credit, and while out of credit it keeps just the *newest* frame for that
/// surface. In-flight depth is therefore bounded and each client's rate tracks
/// the browser's actual presentation rate rather than running ahead into a
/// growing queue.
///
/// Everything is keyed by surface. A single pending slot shared by the whole
/// desktop looks right with one window and quietly starves every window but one
/// as soon as there are two: each surface's held frame is overwritten by the
/// next surface to produce anything, so the others go blank and their titles
/// never arrive.
#[derive(Debug)]
struct Pacer {
    credit: HashMap<u64, i32>,
    pending: HashMap<u64, ServerMessage>,
}

impl Pacer {
    fn new() -> Self {
        Self {
            credit: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    /// A frame arrived from the compositor; returns what to send now, if any.
    fn on_frame(&mut self, message: ServerMessage) -> Option<ServerMessage> {
        // Only frames are paced. Announcements, titles and destructions are
        // small, rare, and useless late — a held title is a window captioned
        // with a placeholder for as long as it stays still.
        let ServerMessage::SurfaceFrame(frame) = &message else {
            return Some(message);
        };
        let id = frame.id.0;
        let credit = self.credit.entry(id).or_insert(INITIAL_CREDIT);
        if *credit > 0 {
            *credit -= 1;
            Some(message)
        } else {
            // Drop this surface's stale frame, keep its newest.
            self.pending.insert(id, message);
            None
        }
    }

    /// The browser presented a frame of `id`; returns the next one to send.
    fn on_ack(&mut self, id: u64) -> Option<ServerMessage> {
        let credit = self.credit.entry(id).or_insert(INITIAL_CREDIT);
        *credit += 1;
        let next = self.pending.remove(&id);
        if next.is_some() {
            *credit -= 1;
        }
        next
    }
}

/// Fan-out of compositor frames to every connected browser.
///
/// Cloneable and runtime-free to `emit` from, so the compositor's synchronous
/// render loop can push [`ServerMessage`]s straight into it.
#[derive(Clone, Debug)]
pub struct FrameSink {
    frames: broadcast::Sender<ServerMessage>,
}

impl Default for FrameSink {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameSink {
    /// Create a sink with a bounded backlog; a browser that falls too far behind
    /// drops frames (it will be paced properly in Phase 2's backpressure work).
    #[must_use]
    pub fn new() -> Self {
        let (frames, _) = broadcast::channel(256);
        Self { frames }
    }

    /// Push a frame to all connected browsers. Cheap when there are none.
    pub fn emit(&self, message: ServerMessage) {
        let _ = self.frames.send(message);
    }

    fn subscribe(&self) -> broadcast::Receiver<ServerMessage> {
        self.frames.subscribe()
    }
}

/// A connected browser: push [`ServerMessage`]s out, pull [`ClientMessage`]s in.
///
/// Encoding/decoding and socket I/O run on background tasks; this handle just
/// moves typed messages across channels.
#[derive(Debug)]
pub struct Connection {
    outgoing: mpsc::UnboundedSender<ServerMessage>,
    incoming: mpsc::UnboundedReceiver<ClientMessage>,
}

impl Connection {
    /// Queue a message for the browser. Returns `false` if the connection is gone.
    pub fn send(&self, message: ServerMessage) -> bool {
        self.outgoing.send(message).is_ok()
    }

    /// Await the next input from the browser, or `None` once it disconnects.
    pub async fn recv(&mut self) -> Option<ClientMessage> {
        self.incoming.recv().await
    }
}

/// Bind a listening socket. Callers should pass a `127.0.0.1` address.
pub async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

/// Accept one browser, upgrade it to WebSocket, and return a [`Connection`].
///
/// Spawns a reader task (frame → `decode` → incoming) and a writer task
/// (outgoing → `encode` → frame).
pub async fn accept(
    listener: &TcpListener,
) -> Result<Connection, Box<dyn std::error::Error + Send + Sync>> {
    let (stream, peer) = listener.accept().await?;
    let ws = tokio_tungstenite::accept_async(stream).await?;
    tracing::info!(%peer, "browser connected");
    let (mut writer, mut reader) = ws.split();

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerMessage>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<ClientMessage>();

    tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            match encode(&message) {
                Ok(bytes) => {
                    if writer.send(WsMessage::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                Err(err) => tracing::error!(%err, "failed to encode outgoing frame"),
            }
        }
    });

    tokio::spawn(async move {
        while let Some(Ok(frame)) = reader.next().await {
            if let WsMessage::Binary(bytes) = frame {
                match decode::<ClientMessage>(bytes.as_ref()) {
                    Ok(message) => {
                        if in_tx.send(message).is_err() {
                            break;
                        }
                    }
                    Err(err) => tracing::warn!(%err, "dropping undecodable frame"),
                }
            }
        }
    });

    Ok(Connection {
        outgoing: out_tx,
        incoming: in_rx,
    })
}

/// Run a WebSocket server on a background thread.
///
/// Each connected browser receives every frame the compositor pushes into
/// `sink`, and everything it sends back — input, and the frame acks that pace
/// `wl_surface.frame` (Decision 3) — is forwarded to the compositor on `client`.
/// Enabled via the `WEBLAND_WS` env var so it never interferes with the window.
pub fn spawn_server(
    addr: SocketAddr,
    sink: FrameSink,
    client: mpsc::UnboundedSender<ClientMessage>,
) {
    let spawned = std::thread::Builder::new()
        .name("webland-ws".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    tracing::error!(%err, "failed to build websocket runtime");
                    return;
                }
            };

            runtime.block_on(async move {
                let listener = match bind(addr).await {
                    Ok(listener) => listener,
                    Err(err) => {
                        tracing::error!(%err, %addr, "failed to bind websocket transport");
                        return;
                    }
                };
                tracing::info!(%addr, "websocket transport listening");

                loop {
                    match accept(&listener).await {
                        Ok(mut connection) => {
                            let mut frames = sink.subscribe();
                            let client_tx = client.clone();
                            tokio::spawn(async move {
                                let mut pacer = Pacer::new();
                                loop {
                                    tokio::select! {
                                        incoming = connection.recv() => match incoming {
                                            // The ack both releases a withheld
                                            // frame here and credits the
                                            // compositor's frame clock.
                                            Some(ClientMessage::FramePresented { id }) => {
                                                let _ =
                                                    client_tx.send(ClientMessage::FramePresented { id });
                                                if let Some(frame) = pacer.on_ack(id.0)
                                                    && !connection.send(frame)
                                                {
                                                    break;
                                                }
                                            }
                                            Some(message) => {
                                                let _ = client_tx.send(message);
                                            }
                                            None => break,
                                        },
                                        frame = frames.recv() => match frame {
                                            Ok(message) => {
                                                if let Some(out) = pacer.on_frame(message)
                                                    && !connection.send(out)
                                                {
                                                    break;
                                                }
                                            }
                                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                                            Err(broadcast::error::RecvError::Closed) => break,
                                        },
                                    }
                                }
                                tracing::info!("browser disconnected");
                            });
                        }
                        Err(err) => tracing::warn!(%err, "websocket accept failed"),
                    }
                }
            });
        });

    if let Err(err) = spawned {
        tracing::error!(%err, "failed to spawn websocket thread");
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameSink, accept, bind};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use webland_core::{Point, Size, SurfaceId};
    use webland_protocol::{
        ClientMessage, InputEvent, ServerMessage, SurfaceCreated, decode, encode,
    };

    fn frame(id: u64) -> ServerMessage {
        tagged(id, 0)
    }

    /// A frame of `id` carrying `tag`, so one can be told from another.
    fn tagged(id: u64, tag: u8) -> ServerMessage {
        ServerMessage::SurfaceFrame(webland_protocol::SurfaceFrame {
            id: SurfaceId(id),
            codec: webland_protocol::Codec::Raw,
            damage: Vec::new(),
            payload: vec![tag],
        })
    }

    #[test]
    fn pacer_bounds_inflight_and_keeps_newest() {
        use super::{INITIAL_CREDIT, Pacer};
        let mut pacer = Pacer::new();

        // Up to INITIAL_CREDIT frames go out before any ack is required.
        for _ in 0..INITIAL_CREDIT {
            assert!(pacer.on_frame(tagged(1, 0)).is_some());
        }
        // Out of credit: further frames are withheld, only the newest retained.
        assert!(pacer.on_frame(tagged(1, 7)).is_none());
        assert!(pacer.on_frame(tagged(1, 9)).is_none());

        // An ack releases exactly the newest withheld frame, not the stale one.
        match pacer.on_ack(1) {
            Some(ServerMessage::SurfaceFrame(f)) => assert_eq!(f.payload, vec![9]),
            other => panic!("expected the newest withheld frame, got {other:?}"),
        }
        // Nothing pending now: the next ack releases nothing.
        assert!(pacer.on_ack(1).is_none());
    }

    #[test]
    fn pacer_starves_no_surface() {
        use super::{INITIAL_CREDIT, Pacer};
        let mut pacer = Pacer::new();

        // One busy window must not spend another's credit, and must not
        // overwrite what another has waiting. A single pending slot for the
        // whole desktop did both: every window but the busiest went blank.
        for _ in 0..INITIAL_CREDIT {
            assert!(pacer.on_frame(frame(1)).is_some());
            assert!(pacer.on_frame(frame(2)).is_some());
        }
        assert!(pacer.on_frame(tagged(1, 11)).is_none());
        assert!(pacer.on_frame(tagged(2, 22)).is_none());

        // Each surface gets its own frame back, not the other's.
        match pacer.on_ack(2) {
            Some(ServerMessage::SurfaceFrame(f)) => {
                assert_eq!(f.id, SurfaceId(2));
                assert_eq!(f.payload, vec![22]);
            }
            other => panic!("surface 2 should get its own held frame, got {other:?}"),
        }
        match pacer.on_ack(1) {
            Some(ServerMessage::SurfaceFrame(f)) => {
                assert_eq!(f.id, SurfaceId(1));
                assert_eq!(f.payload, vec![11]);
            }
            other => panic!("surface 1's held frame should have survived, got {other:?}"),
        }
    }

    #[test]
    fn pacer_lets_titles_through_while_out_of_credit() {
        use super::Pacer;
        let mut pacer = Pacer::new();
        while pacer.on_frame(frame(1)).is_some() {}
        // A title held until the window next redraws is a window captioned
        // with a placeholder for as long as it sits still.
        let title = ServerMessage::SurfaceTitle {
            id: SurfaceId(1),
            title: String::from("editor"),
        };
        assert!(pacer.on_frame(title).is_some());
    }

    #[test]
    fn pacer_lets_surface_announcements_bypass() {
        use super::Pacer;
        let mut pacer = Pacer::new();
        // Exhaust credit.
        while pacer.on_frame(frame(1)).is_some() {}
        // A SurfaceCreated still goes out immediately despite zero credit.
        let created = ServerMessage::SurfaceCreated(SurfaceCreated {
            id: SurfaceId(9),
            size: Size {
                width: 1,
                height: 1,
            },
        });
        assert!(pacer.on_frame(created).is_some());
    }

    #[tokio::test]
    async fn frame_sink_fans_out_to_subscribers() {
        let sink = FrameSink::new();
        let mut a = sink.subscribe();
        let mut b = sink.subscribe();

        let frame = ServerMessage::SurfaceCreated(SurfaceCreated {
            id: SurfaceId(1),
            size: Size {
                width: 10,
                height: 20,
            },
        });
        sink.emit(frame.clone());

        assert_eq!(a.recv().await.unwrap(), frame);
        assert_eq!(b.recv().await.unwrap(), frame);
    }

    // A real loopback round-trip over TCP + WebSocket, exercising the codec on
    // both ends exactly as the browser will.
    #[tokio::test]
    async fn frames_round_trip_over_websocket() {
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let mut connection = accept(&listener).await.unwrap();
            let input = connection.recv().await.unwrap();
            connection.send(ServerMessage::SurfaceCreated(SurfaceCreated {
                id: SurfaceId(7),
                size: Size {
                    width: 1920,
                    height: 1080,
                },
            }));
            input
        });

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();

        let sent = ClientMessage::Input(InputEvent::PointerMotion {
            position: Point { x: 1.0, y: 2.0 },
        });
        ws.send(WsMessage::Binary(encode(&sent).unwrap()))
            .await
            .unwrap();

        let reply = loop {
            if let WsMessage::Binary(bytes) = ws.next().await.unwrap().unwrap() {
                break decode::<ServerMessage>(bytes.as_ref()).unwrap();
            }
        };

        assert_eq!(server.await.unwrap(), sent);
        assert!(
            matches!(reply, ServerMessage::SurfaceCreated(created) if created.id == SurfaceId(7))
        );
    }
}
