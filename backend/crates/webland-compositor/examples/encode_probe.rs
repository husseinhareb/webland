//! Does the encoder produce a real H.264 stream, and at what bitrate?
//!
//! Phase 2 gate 4 wants 1080p60 inside a sane video bitrate, so this encodes
//! five seconds of moving 1080p and writes Annex B to a file. Decoding that file
//! with ffmpeg is the other half of the check: a stream that only we can read is
//! no use to a browser.

use webland_compositor::encode::{Encoded, Encoder, Input};

fn main() {
    let (w, h) = (1920u32, 1080u32);
    let node = std::env::var("WEBLAND_RENDER_NODE")
        .unwrap_or_else(|_| String::from("/dev/dri/renderD128"));
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| String::from("probe.h264"));
    // CPU input: this probe measures the encoder, not the import path.
    let mut encoder = Encoder::new(&node, w, h, 8_000_000, Input::Cpu, 0, 0).expect("encoder");

    let (wide, tall) = (w as usize, h as usize);
    let mut frame = vec![0u8; wide * tall * 4];
    let mut bytes = 0usize;
    let frames = 300usize;
    let start = std::time::Instant::now();
    let mut stream = Vec::new();
    for n in 0..frames {
        // Something that actually moves, so this measures inter-frame coding
        // rather than how well the encoder handles a still image.
        let bar = (n * 6) % wide;
        for y in 0..tall {
            for x in 0..wide {
                let i = (y * wide + x) * 4;
                let lit = x.abs_diff(bar) < 60;
                frame[i] = if lit { 0xF0 } else { 0x20 };
                frame[i + 1] = u8::try_from((y * 255 / tall) & 0xFF).unwrap_or(0);
                frame[i + 2] = if lit { 0x40 } else { 0x80 };
                frame[i + 3] = 0xFF;
            }
        }
        if let Encoded::Packet(packet) = encoder.encode(&frame, n == 0) {
            bytes += packet.len();
            stream.extend_from_slice(&packet);
        }
    }
    let elapsed = start.elapsed();
    std::fs::write(&out, &stream).expect("write stream");

    #[allow(clippy::cast_precision_loss)]
    let seconds = frames as f64 / 60.0;
    #[allow(clippy::cast_precision_loss)]
    let kbit = (bytes as f64 * 8.0 / seconds) / 1000.0;
    #[allow(clippy::cast_precision_loss)]
    let fps = frames as f64 / elapsed.as_secs_f64();
    println!(
        "{frames} frames of {w}x{h} in {:.1}s wall ({fps:.0} fps encode)",
        elapsed.as_secs_f64(),
    );
    println!("{bytes} bytes = {kbit:.0} kbit/s at 60fps -> {out}");
}
