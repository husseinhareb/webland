//! Can this machine do the zero-copy path at all?
//!
//! Opens the render node, brings up EGL on it and prints the dmabuf formats we
//! could advertise through `linux-dmabuf-v1`. If this fails there is no point
//! wiring the global, so it is worth its twenty lines before the three hundred.

#![allow(unsafe_code)] // EGLDisplay::new is unsafe; see the SAFETY note below.

use std::fs::File;

use smithay::backend::allocator::gbm::GbmDevice;
use smithay::backend::egl::EGLDisplay;

fn main() {
    let path = std::env::var("WEBLAND_RENDER_NODE")
        .unwrap_or_else(|_| String::from("/dev/dri/renderD128"));
    let file = File::options()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("open {path}: {e}"));
    let gbm = GbmDevice::new(file).expect("gbm device");
    // SAFETY: `gbm` outlives the display; we never hand its fd to anything else.
    let egl = unsafe { EGLDisplay::new(gbm) }.expect("egl display");

    println!("{path}: EGL up");
    let texture = egl.dmabuf_texture_formats();
    let render = egl.dmabuf_render_formats();
    println!(
        "{} texture formats, {} render formats",
        texture.iter().count(),
        render.iter().count()
    );
    let mut codes: Vec<String> = texture.iter().map(|f| format!("{:?}", f.code)).collect();
    codes.sort();
    codes.dedup();
    println!("codes: {}", codes.join(" "));
}
