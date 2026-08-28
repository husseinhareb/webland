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

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use webland_protocol::{ClientMessage, InputEvent, ServerMessage, decode, encode};

/// How many frames may be in flight to a browser before it must ack. Small, so
/// latency stays low; >1 so the pipeline does not stall on a single round trip.
const INITIAL_CREDIT: i32 = 2;

/// Per-connection frame pacing (Decision 3).
///
/// The browser acks every presented frame; the server sends only while it has
/// credit, and while out of credit it keeps just the *newest* frame. In-flight
/// depth is therefore bounded and the client's rate tracks the browser's actual
/// presentation rate instead of running ahead into a growing queue.
#[derive(Debug)]
struct Pacer {
    credit: i32,
    pending: Option<ServerMessage>,
}

impl Pacer {
    fn new() -> Self {
        Self {
            credit: INITIAL_CREDIT,
            pending: None,
        }
    }

    /// A frame arrived from the compositor; returns what to send now, if any.
    fn on_frame(&mut self, message: ServerMessage) -> Option<ServerMessage> {
        // Control messages (surface announcements) bypass pacing.
        if matches!(message, ServerMessage::SurfaceCreated(_)) {
            return Some(message);
        }
        if self.credit > 0 {
            self.credit -= 1;
            Some(message)
        } else {
            self.pending = Some(message); // drop the stale one, keep the newest
            None
        }
    }

    /// The browser presented a frame; returns the next frame to send, if any.
    fn on_ack(&mut self) -> Option<ServerMessage> {
        self.credit += 1;
        let next = self.pending.take();
        if next.is_some() {
            self.credit -= 1;
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
    let (stream, _peer) = listener.accept().await?;
    let ws = tokio_tungstenite::accept_async(stream).await?;
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
/// `sink`, and its input is logged (routed to the compositor in Phase 3).
/// Enabled via the `WEBLAND_WS` env var so it never interferes with the window.
pub fn spawn_server(addr: SocketAddr, sink: FrameSink, input: mpsc::UnboundedSender<InputEvent>) {
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
                            let input_tx = input.clone();
                            tokio::spawn(async move {
                                let mut pacer = Pacer::new();
                                loop {
                                    tokio::select! {
                                        input = connection.recv() => match input {
                                            Some(ClientMessage::FramePresented) => {
                                                if let Some(frame) = pacer.on_ack()
                                                    && !connection.send(frame)
                                                {
                                                    break;
                                                }
                                            }
                                            Some(ClientMessage::Input(event)) => {
                                                let _ = input_tx.send(event);
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
        ServerMessage::SurfaceFrame(webland_protocol::SurfaceFrame {
            id: SurfaceId(id),
            codec: webland_protocol::Codec::Raw,
            damage: Vec::new(),
            payload: Vec::new(),
        })
    }

    #[test]
    fn pacer_bounds_inflight_and_keeps_newest() {
        use super::{INITIAL_CREDIT, Pacer};
        let mut pacer = Pacer::new();

        // Up to INITIAL_CREDIT frames go out before any ack is required.
        for _ in 0..INITIAL_CREDIT {
            assert!(pacer.on_frame(frame(1)).is_some());
        }
        // Out of credit: further frames are withheld, only the newest retained.
        assert!(pacer.on_frame(frame(2)).is_none());
        assert!(pacer.on_frame(frame(3)).is_none());

        // An ack releases exactly the newest withheld frame (id 3, not 2).
        match pacer.on_ack() {
            Some(ServerMessage::SurfaceFrame(f)) => assert_eq!(f.id, SurfaceId(3)),
            other => panic!("expected withheld frame 3, got {other:?}"),
        }
        // Nothing pending now: the next ack releases nothing.
        assert!(pacer.on_ack().is_none());
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
