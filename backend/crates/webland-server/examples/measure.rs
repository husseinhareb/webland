//! Measure what the frame stream actually costs on the wire, and check that it
//! reconstructs.
//!
//! Phase 2 is not done until its numbers are met (see `docs/roadmap.md`), so
//! this connects exactly as the browser does — asks for a keyframe, applies each
//! damage rectangle to its own copy of the surface, acks every frame so the
//! compositor's frame clock keeps turning — and reports the rate. At the end it
//! asks for one more keyframe and compares: with an idle client its copy must be
//! byte-identical, or the browser's would be quietly wrong too.
//!
//! ```sh
//! WEBLAND_SPAWN=kitty ./scripts/dev.sh &
//! cargo run --manifest-path backend/Cargo.toml -p webland-server --example measure -- 10
//! ```
use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use webland_core::{Size, SurfaceId};
use webland_protocol::{
    ClientMessage, Codec, ServerMessage, SurfaceFrame, decode, encode, inflate,
};

/// One surface as the browser would hold it.
struct Surface {
    size: Size,
    pixels: Vec<u8>,
}

impl Surface {
    fn new(size: Size) -> Self {
        Self {
            size,
            pixels: vec![0; size.width as usize * size.height as usize * 4],
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(10);
    let url = std::env::var("WEBLAND_WS_URL").unwrap_or_else(|_| "ws://127.0.0.1:9001".to_string());

    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await?;
    socket
        .send(Message::Binary(encode(&ClientMessage::RequestKeyframe)?))
        .await?;

    let start = Instant::now();
    let (mut frames, mut bytes, mut pixels) = (0u64, 0u64, 0u64);
    let mut surfaces: HashMap<SurfaceId, Surface> = HashMap::new();
    // Frames that cannot be applied — damage outside the surface, or a payload
    // that does not fill it — corrupt the browser's texture silently.
    let mut unapplicable = 0u64;
    // Set once the closing keyframe has been compared against our own copy.
    let mut verdict = String::from("no keyframe to check against");
    // H.264 frames carry no damage and are not reconstructable here: checking
    // them would mean decoding video, which is the browser's job.
    let mut encoded = 0u64;

    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    let mut checking = false;
    loop {
        tokio::select! {
            () = &mut deadline => {
                if checking {
                    break;
                }
                // Ask for whole surfaces, then compare them against what the
                // damage rectangles alone built.
                checking = true;
                socket.send(Message::Binary(encode(&ClientMessage::RequestKeyframe)?)).await?;
                deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(2));
            }
            message = socket.next() => {
                let Some(Ok(Message::Binary(frame))) = message else { break };
                bytes += frame.len() as u64;
                match decode::<ServerMessage>(&frame) {
                    Ok(ServerMessage::SurfaceCreated(created)) => {
                        surfaces
                            .entry(created.id)
                            .and_modify(|surface| {
                                if surface.size != created.size {
                                    *surface = Surface::new(created.size);
                                }
                            })
                            .or_insert_with(|| Surface::new(created.size));
                    }
                    Ok(ServerMessage::SurfaceFrame(frame)) => {
                        frames += 1;
                        if frame.codec == Codec::H264 {
                            encoded += 1;
                        }
                        pixels += frame
                            .damage
                            .iter()
                            .map(|rect| u64::from(rect.width) * u64::from(rect.height))
                            .sum::<u64>();
                        match apply(&mut surfaces, &frame, checking) {
                            Ok(Some(difference)) => verdict = difference,
                            Ok(None) => {}
                            Err(()) => unapplicable += 1,
                        }
                        socket
                            .send(Message::Binary(encode(&ClientMessage::FramePresented {
                                id: frame.id,
                            })?))
                            .await?;
                        if checking && (verdict.starts_with("reconstruct") || encoded > 0) {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    #[allow(clippy::cast_precision_loss)]
    let (frames_f, bytes_f, pixels_f) = (frames as f64, bytes as f64, pixels as f64);
    println!(
        "{frames} frames in {elapsed:.1}s — {:.1} frames/s, {:.1} KiB/s, {:.0} px/frame damage",
        frames_f / elapsed,
        bytes_f / 1024.0 / elapsed,
        if frames == 0 {
            0.0
        } else {
            pixels_f / frames_f
        },
    );
    if encoded > 0 {
        println!("{encoded} of {frames} frames were H.264 (bitrate above is the real number)");
        println!("{unapplicable} unapplicable frames, damage reconstruction n/a for video");
    } else {
        println!("{unapplicable} unapplicable frames, {verdict}");
    }
    Ok(())
}

/// Blit a frame into our copy of its surface, exactly as the browser does.
///
/// When `check` is set and the frame covers a whole surface, the frame is
/// compared against what the damage rectangles built rather than trusted, and
/// the difference reported.
fn apply(
    surfaces: &mut HashMap<SurfaceId, Surface>,
    frame: &SurfaceFrame,
    check: bool,
) -> Result<Option<String>, ()> {
    let payload = match frame.codec {
        Codec::Deflate => inflate(&frame.payload).map_err(|_| ())?,
        Codec::Raw => frame.payload.clone(),
        // Nothing to reconstruct from until the WebCodecs path exists.
        Codec::H264 => return Ok(None),
    };
    let surface = surfaces.get_mut(&frame.id).ok_or(())?;
    let [region] = frame.damage[..] else {
        return Err(());
    };
    let (Ok(x), Ok(y)) = (usize::try_from(region.x), usize::try_from(region.y)) else {
        return Err(());
    };
    let (width, height) = (region.width as usize, region.height as usize);
    let stride = surface.size.width as usize * 4;
    if x + width > surface.size.width as usize
        || y + height > surface.size.height as usize
        || payload.len() != width * height * 4
    {
        return Err(());
    }

    let whole = width * 4 == stride && height == surface.size.height as usize;
    let mut difference = None;
    if check && whole {
        let differing = payload
            .iter()
            .zip(&surface.pixels)
            .filter(|(sent, held)| sent != held)
            .count();
        difference = Some(if differing == 0 {
            "reconstructed exactly from damage".to_string()
        } else {
            format!(
                "reconstruction differs in {differing} of {} bytes (expected while a client is drawing)",
                payload.len()
            )
        });
    }
    for row in 0..height {
        let start = (y + row) * stride + x * 4;
        surface.pixels[start..start + width * 4]
            .copy_from_slice(&payload[row * width * 4..(row + 1) * width * 4]);
    }
    Ok(difference)
}
