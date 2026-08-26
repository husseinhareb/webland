//! Webland server: hosts the compositor and serves the browser frontend.

#[tokio::main]
async fn main() -> webland_core::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!(
        protocol = webland_protocol::VERSION,
        "webland: architectural skeleton, nothing to run yet"
    );
    Ok(())
}
