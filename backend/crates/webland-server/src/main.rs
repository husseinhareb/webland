//! Webland server: hosts the compositor and (later) serves the browser frontend.
//!
//! Phase 2 (in progress): a WebSocket transport carries `webland-protocol`
//! frames from the compositor to the browser, and carries input and frame acks
//! back. Set `WEBLAND_WS=127.0.0.1:PORT` to bring it up; `WEBLAND_HEADLESS`
//! drops the local window so the browser is the only display.

mod audio;
mod auth;
mod notify;
mod transport;
mod tray;
mod wallpaper;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    tracing::info!(protocol = webland_protocol::VERSION, "webland: starting");

    // The session's own bus, started before anything that speaks to it. The
    // applications the compositor launches register their tray icons here, and
    // the tray watches this same bus for exactly those registrations; the
    // host's bus belongs to the host's desktop, where the watcher name is taken
    // and where our applications are not.
    let bus = webland_compositor::spawn::Bus::start();
    if let Some(bus) = &bus {
        tracing::info!(address = %bus.address(), "session bus");
    } else {
        tracing::warn!(
            "no session bus: applications may open on the host desktop, and there is no tray"
        );
    }

    let (on_frame, poll_input, wake) = if let Some(addr) = std::env::var("WEBLAND_WS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        let token = auth::load()?;
        // The page is served on its own port (trunk's, by default), and the
        // token travels in the fragment, so it never reaches a request line.
        let page = std::env::var("WEBLAND_PORT").unwrap_or_else(|_| String::from("3030"));
        tracing::info!("open http://127.0.0.1:{page}/#token={token}");
        let sink = transport::FrameSink::new();
        let (client_tx, mut client_rx) = tokio::sync::mpsc::unbounded_channel();
        transport::spawn_server(
            addr,
            sink.clone(),
            client_tx,
            bus.as_ref().map(|bus| bus.address().to_owned()),
            token,
        );

        let on_frame = Box::new(move |message| sink.emit(message)) as Box<dyn Fn(_)>;
        // Relayed so each message also pings the compositor's loop awake: it
        // waits on events, and a tokio channel is not one it can wait on.
        let (ping, wake) = webland_compositor::make_ping()?;
        let (relay_tx, relay_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("webland-input".to_owned())
            .spawn(move || {
                while let Some(message) = client_rx.blocking_recv() {
                    if relay_tx.send(message).is_err() {
                        break;
                    }
                    ping.ping();
                }
            })?;
        let poll_client =
            Box::new(move || relay_rx.try_recv().ok()) as Box<dyn FnMut() -> Option<_>>;
        (Some(on_frame), Some(poll_client), Some(wake))
    } else {
        (None, None, None)
    };

    // The session's own audio sink, created before the compositor starts so
    // that everything it launches finds it. Held to the end of `main`: dropping
    // it takes the sink away again.
    let _sink = audio::Sink::create();

    // Headless makes the browser the only display; winit keeps a local window
    // (a visual ground-truth for debugging). Default to winit unless asked.
    if std::env::var_os("WEBLAND_HEADLESS").is_some() {
        webland_compositor::run_headless(on_frame, poll_input, wake, bus.as_ref())
    } else {
        webland_compositor::run_winit(on_frame, poll_input, bus.as_ref())
    }
}
