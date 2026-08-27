//! Webland frontend entry point.
//!
//! Rust + Leptos, compiled to WebAssembly by Trunk. The frontend is WASM so
//! that `webland-protocol` is shared verbatim with the backend and the wire
//! format cannot drift between the two sides.

mod compositor;
mod desktop;
mod gpu;
mod protocol;

use desktop::Desktop;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(Desktop);
}
