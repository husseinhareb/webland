//! The session's audio, on its way to the browser.
//!
//! Webland's applications play into a null sink of the session's own
//! ([`webland_compositor::spawn::AUDIO_SINK`]), never into the machine's
//! speakers — the desktop is being watched somewhere else, possibly on another
//! machine, and that is where its sound belongs. This module creates that sink
//! and streams its monitor to each connected browser.
//!
//! The encode is `ffmpeg`'s, as a child process: pulse in, Opus out, wrapped in
//! `WebM` so the browser can hand it straight to a `MediaSource` without a codec
//! configuration of our own. One capture per browser, because the first bytes
//! of that stream are its header and a browser joining a stream already in
//! progress would have nothing to start its decoder from. Nothing runs at all
//! while nobody is watching.
//!
//! ponytail: `WebM` through `MediaSource` buffers, so this is a media path
//! (a video, a music player) and not a low-latency one — expect a few hundred
//! milliseconds. Raw Opus packets into `WebCodecs`' `AudioDecoder` is the
//! upgrade if a click ever has to be heard the instant it is made.

use std::io::Read;
use std::process::{Child, Command, Stdio};

use tokio::sync::mpsc::UnboundedSender;
use webland_compositor::spawn::AUDIO_SINK;
use webland_protocol::ServerMessage;

/// How much of the stream to send at a time. Small enough that a chunk is never
/// worth waiting to fill, large enough not to spend a protocol message on every
/// few samples.
const CHUNK: usize = 4096;

/// The session's null sink, for as long as the server runs.
#[derive(Debug)]
pub struct Sink {
    /// The id `pactl` gave the module, so it can be unloaded again. A sink left
    /// behind would show up in the machine's mixer for the rest of the login.
    module: String,
}

impl Sink {
    /// Create the session's sink.
    ///
    /// Returns `None` when there is no `PulseAudio` (or `PipeWire`'s pulse server)
    /// to create it in, which is not fatal: the desktop runs, and applications
    /// fall back to whatever `PULSE_SINK` means to a machine that has no such
    /// sink — nothing, so they play on the host's own output as they did
    /// before.
    #[must_use]
    pub fn create() -> Option<Self> {
        let output = Command::new("pactl")
            .args([
                "load-module",
                "module-null-sink",
                &format!("sink_name={AUDIO_SINK}"),
                "sink_properties=device.description=Webland",
            ])
            .output()
            .inspect_err(|err| tracing::warn!(%err, "no pactl: session audio stays on the host"))
            .ok()?;
        if !output.status.success() {
            tracing::warn!(
                status = ?output.status,
                "could not create the session's audio sink"
            );
            return None;
        }
        let module = String::from_utf8_lossy(&output.stdout).trim().to_string();
        tracing::info!(sink = AUDIO_SINK, %module, "session audio sink");
        Some(Self { module })
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        let _ = Command::new("pactl")
            .args(["unload-module", &self.module])
            .status();
    }
}

/// One browser's audio stream. Dropping it stops the capture.
#[derive(Debug)]
pub struct Capture {
    ffmpeg: Child,
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.ffmpeg.kill();
        let _ = self.ffmpeg.wait();
    }
}

/// Start streaming the session's sink to one browser.
///
/// Returns `None` if `ffmpeg` is not installed or cannot open the sink, in
/// which case the desktop is simply silent.
#[must_use]
pub fn capture(to_browser: UnboundedSender<ServerMessage>) -> Option<Capture> {
    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "pulse",
            "-i",
            &format!("{AUDIO_SINK}.monitor"),
            "-ac",
            "2",
            "-ar",
            "48000",
            "-c:a",
            "libopus",
            "-b:a",
            "96k",
            // 20 ms frames and 100 ms clusters: the muxer holds a cluster back
            // until it is full, so this is most of the latency and all of the
            // reason not to make it smaller still.
            "-frame_duration",
            "20",
            "-f",
            "webm",
            "-cluster_time_limit",
            "100",
            "-live",
            "1",
            "-flush_packets",
            "1",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .inspect_err(|err| tracing::warn!(%err, "no ffmpeg: the desktop will be silent"))
        .ok()?;
    let mut stdout = ffmpeg.stdout.take()?;

    // A blocking read on a thread of its own: this is a pipe that fills at the
    // speed of sound, and an async runtime has better things to wait on.
    let spawned = std::thread::Builder::new()
        .name("webland-audio".to_owned())
        .spawn(move || {
            let mut buffer = [0u8; CHUNK];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if to_browser
                            .send(ServerMessage::Audio {
                                payload: buffer[..read].to_vec(),
                            })
                            .is_err()
                        {
                            // The browser is gone; so is the reason to encode.
                            break;
                        }
                    }
                    Err(err) => {
                        tracing::warn!(%err, "audio capture ended");
                        break;
                    }
                }
            }
        });
    if let Err(err) = spawned {
        tracing::error!(%err, "failed to spawn the audio thread");
        let _ = ffmpeg.kill();
        return None;
    }
    Some(Capture { ffmpeg })
}
