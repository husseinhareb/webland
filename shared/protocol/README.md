# Webland protocol

The single source of truth for messages exchanged between the Rust backend and
the browser frontend. The wire format is implemented once in the
`backend/crates/webland-protocol` crate; because the Leptos frontend also
compiles to WebAssembly, it depends on that same crate rather than
reimplementing the codec, so backend and frontend can never drift.
`frontend/src/protocol` adds only the browser-side transport seam over it.
Neither side invents messages on its own — this document specifies them.

## Decisions (Phase 0)

These are cheap to record now and enormously expensive to change in Phase 5.

1. **Per-surface streaming, not desktop streaming.** Each Wayland surface is its
   own stream; the browser composites. Window movement, stacking, resizing and
   workspace switching happen browser-side at browser frame rate and never touch
   the server. This is the one choice that makes Webland structurally different
   from VNC. The cost is N encoded streams instead of one.

2. **The pixel path never touches the CPU.** A client dmabuf is VA-API encoded
   directly on the GPU, sent as a bitstream, decoded by WebCodecs, and imported
   as a WebGPU external texture — no readback on either side. `wl_shm` clients
   hand over CPU buffers instead; that path exists too and is the easy one.
   Video is not a special case: a terminal is just a highly compressible video.

3. **The browser drives the frame clock.** Wayland clients redraw only when the
   compositor fires `wl_surface.frame`. Client frame rate must be paced by the
   browser's actual presentation rate, or an unbounded queue forms and latency
   grows without limit. This is solved in Phase 2, not deferred.

## Messages (Phase 0)

Three messages, enough to force the decisions above into the open. Defined as
Rust types in `webland-protocol`; shown here in shape, not wire bytes.

- `SurfaceCreated { id, size }` — a surface appeared; allocate a scene node.
- `SurfaceFrame { id, codec, damage, payload }` — new contents for a surface.
- `InputEvent { … }` — pointer/keyboard input from the browser to a client.

Direction is captured by two envelopes: `ServerMessage` (backend → browser,
carrying `SurfaceCreated` / `SurfaceFrame`) and `ClientMessage` (browser →
backend, carrying `InputEvent`).

## Encoding (decided, Phase 2)

The wire encoding is **`bincode`**: `encode`/`decode` in `webland-protocol` turn
a `ServerMessage`/`ClientMessage` into and out of a single frame. A transport
delivers whole frames (WebSocket messages are already framed), so no length
prefix is imposed here. The same functions run on both ends because the frontend
links the crate.

## Still open, deliberately unanswered

- **Codec set** — the *pixel* `Codec` (`Raw`, `H264`) is separate from the
  message encoding above; `Raw` and `H264` are placeholders and the real set
  follows from what VA-API and WebCodecs agree on.
- **Transport** — WebSocket first, WebTransport later; the message set must not
  depend on either.
