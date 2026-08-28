//! Webland server: hosts the compositor and (later) serves the browser frontend.
//!
//! Phase 2 (in progress): a WebSocket transport carries `webland-protocol`
//! frames from the compositor to the browser, and carries input and frame acks
//! back. Set `WEBLAND_WS=127.0.0.1:PORT` to bring it up; `WEBLAND_HEADLESS`
//! drops the local window so the browser is the only display.

mod transport;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    tracing::info!(protocol = webland_protocol::VERSION, "webland: starting");

    let (on_frame, poll_input) = if let Some(addr) = std::env::var("WEBLAND_WS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        let sink = transport::FrameSink::new();
        let (client_tx, mut client_rx) = tokio::sync::mpsc::unbounded_channel();
        transport::spawn_server(addr, sink.clone(), client_tx);

        let on_frame = Box::new(move |message| sink.emit(message)) as Box<dyn Fn(_)>;
        // `try_recv` is synchronous and runtime-free, so the compositor's own
        // thread can drain browser input and frame acks each iteration.
        let poll_client =
            Box::new(move || client_rx.try_recv().ok()) as Box<dyn FnMut() -> Option<_>>;
        (Some(on_frame), Some(poll_client))
    } else {
        (None, None)
    };

    // Headless makes the browser the only display; winit keeps a local window
    // (a visual ground-truth for debugging). Default to winit unless asked.
    if std::env::var_os("WEBLAND_HEADLESS").is_some() {
        webland_compositor::run_headless(on_frame, poll_input)
    } else {
        webland_compositor::run_winit(on_frame, poll_input)
    }
}
