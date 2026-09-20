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
use webland_protocol::{ClientMessage, ServerMessage, decode, encode};

use crate::audio;

// Frames are not paced here. They are paced at the source, by the compositor's
// per-surface `FrameClock`: it withholds `wl_surface.frame` callbacks until the
// browser acks, so a throttled client never draws and no frame is produced.
//
// This used to hold a credit here too and, when out of credit, drop the older
// frame and keep the newest. That is safe only for self-contained images, and
// nothing sent here is one: an H.264 delta frame references the frame before it,
// and even a deflate frame carries just a damage rectangle. Dropping one leaves
// the decoder applying deltas to a reference that never arrived, which looks
// like half-drawn glyphs smeared across the window — and it only happens under
// fast typing, when the credit runs out. Throttling by discarding is only ever
// correct where a whole picture supersedes the last one.

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

    /// A handle for pushing messages from somewhere other than the frame loop —
    /// the audio capture, which produces on its own thread and at its own pace.
    #[must_use]
    pub fn sender(&self) -> mpsc::UnboundedSender<ServerMessage> {
        self.outgoing.clone()
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

/// Upgrade an accepted socket to WebSocket and return a [`Connection`].
///
/// Spawns a reader task (frame → `decode` → incoming) and a writer task
/// (outgoing → `encode` → frame).
///
/// Separate from accepting the socket because the handshake waits on the peer:
/// it is the peer's HTTP request that is being read. Run inline in the accept
/// loop, one connection that never sends that request — a port scanner, a proxy
/// probe, a browser on a stalled network — holds the loop open and no other
/// browser can connect at all.
async fn upgrade(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
) -> Result<Connection, Box<dyn std::error::Error + Send + Sync>> {
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
                    let (stream, peer) = match listener.accept().await {
                        Ok(accepted) => accepted,
                        Err(err) => {
                            tracing::warn!(%err, "websocket accept failed");
                            continue;
                        }
                    };
                    // Handshake and serve on a task of their own, so the next
                    // browser is accepted while this one is still upgrading.
                    let sink = sink.clone();
                    let client_tx = client.clone();
                    tokio::spawn(async move {
                        let mut connection = match upgrade(stream, peer).await {
                            Ok(connection) => connection,
                            Err(err) => {
                                tracing::warn!(%err, %peer, "websocket handshake failed");
                                return;
                            }
                        };
                        // This browser's own audio stream, for as long as it
                        // is connected: the capture stops when `_audio` drops
                        // at the end of this task.
                        let _audio = audio::capture(connection.sender());
                        // Subscribed once there is somewhere to put the frames:
                        // a subscription nobody reads from fills up and reports
                        // a lag that costs everyone a keyframe.
                        let mut frames = sink.subscribe();
                        loop {
                            tokio::select! {
                                incoming = connection.recv() => match incoming {
                                    // The ack both releases a withheld frame
                                    // here and credits the compositor's frame
                                    // clock.
                                    Some(ClientMessage::FramePresented { id }) => {
                                        let _ = client_tx.send(ClientMessage::FramePresented { id });
                                    }
                                    Some(message) => {
                                        let _ = client_tx.send(message);
                                    }
                                    None => break,
                                },
                                frame = frames.recv() => match frame {
                                    Ok(message) => {
                                        if !connection.send(message) {
                                            break;
                                        }
                                    }
                                    // The broadcast queue overflowed and frames
                                    // were lost. The decoder is now applying
                                    // deltas against a reference it never
                                    // received, so ask for a keyframe rather
                                    // than let it render nonsense until the
                                    // next one happens along.
                                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                                        tracing::warn!(dropped, "browser fell behind; resyncing");
                                        let _ = client_tx.send(ClientMessage::RequestKeyframe);
                                    }
                                    Err(broadcast::error::RecvError::Closed) => break,
                                },
                            }
                        }
                        tracing::info!(%peer, "browser disconnected");
                    });
                }
            });
        });

    if let Err(err) = spawned {
        tracing::error!(%err, "failed to spawn websocket thread");
    }
}

#[cfg(test)]
mod tests {
    use super::{Connection, FrameSink, bind, mpsc, spawn_server, upgrade};
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use webland_core::{Point, Rect, Size, SurfaceId};

    /// Accept one connection and finish its handshake, as the server loop does.
    async fn accept(listener: &TcpListener) -> Connection {
        let (stream, peer) = listener.accept().await.unwrap();
        upgrade(stream, peer).await.unwrap()
    }
    use webland_protocol::{
        ClientMessage, InputEvent, ServerMessage, SurfaceCreated, decode, encode,
    };

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
            content: Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 20,
            },
            parent: None,
            decorated: true,
        });
        sink.emit(frame.clone());

        assert_eq!(a.recv().await.unwrap(), frame);
        assert_eq!(b.recv().await.unwrap(), frame);
    }

    // A real loopback round-trip over TCP + WebSocket, exercising the codec on
    // both ends exactly as the browser will.
    /// A peer that opens a socket and never sends its HTTP request must not
    /// keep the next browser out. The silent socket is left dangling on
    /// purpose: it is exactly what a port scanner or a half-open proxy probe
    /// leaves behind, and the accept loop used to sit in `accept_async` on it.
    ///
    /// Against the real [`spawn_server`], because the loop is the thing being
    /// tested. The port is one the kernel just handed out and we let go of
    /// again, which is as close to reserving one as a test can get.
    #[tokio::test]
    async fn a_silent_peer_does_not_block_the_next_browser() {
        let addr = {
            let scratch = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
            scratch.local_addr().unwrap()
        };
        let (client_tx, _client_rx) = mpsc::unbounded_channel();
        spawn_server(addr, FrameSink::new(), client_tx);

        // The server binds on a thread of its own; wait for the port to answer.
        let silent = loop {
            if let Ok(stream) = tokio::net::TcpStream::connect(addr).await {
                break stream;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };

        let connected = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio_tungstenite::connect_async(format!("ws://{addr}")),
        )
        .await;
        assert!(
            connected.is_ok_and(|result| result.is_ok()),
            "a browser could not connect behind a peer that never handshakes"
        );
        drop(silent);
    }

    #[tokio::test]
    async fn frames_round_trip_over_websocket() {
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let mut connection = accept(&listener).await;
            let input = connection.recv().await.unwrap();
            connection.send(ServerMessage::SurfaceCreated(SurfaceCreated {
                id: SurfaceId(7),
                size: Size {
                    width: 1920,
                    height: 1080,
                },
                content: Rect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
                parent: None,
                decorated: true,
            }));
            input
        });

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();

        let sent = ClientMessage::Input(InputEvent::PointerMotion {
            id: SurfaceId(7),
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
