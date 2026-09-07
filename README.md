# Webland

Webland is an experimental Wayland-based Linux desktop environment whose
graphical display is presented through a web browser.

Linux applications talk Wayland to a Rust backend, which forwards surfaces and
input over the Webland protocol to a browser frontend that draws the desktop
with WebGPU. The frontend is written in Rust with Leptos and compiled to
WebAssembly, so it shares the protocol crate with the backend.

**Status: architectural / prototyping stage.** The repository currently holds
the workspace layout, dependencies and tooling only the compositor, the
protocol and the shell are not implemented. See
[docs/roadmap.md](docs/roadmap.md) for the order of work and what has to be
proven before the desktop gets built.

## Layout

| Path | What lives here |
| --- | --- |
| `backend/` | Rust workspace: compositor, server, protocol, shared core |
| `frontend/` | Rust + Leptos browser desktop, compiled to WebAssembly with Trunk |
| `shared/protocol/` | Language-neutral protocol definition |
| `docs/` | Architecture notes and roadmap |
| `scripts/` | Development helpers |

## Development

The frontend compiles to WebAssembly, so it needs the `wasm32-unknown-unknown`
target plus `trunk` and a matching `wasm-bindgen`.

```sh
# Arch (distro rust, no rustup): versions stay locked to the `rust` package
sudo pacman -S rust-wasm trunk wasm-bindgen

# rustup toolchains
rustup target add wasm32-unknown-unknown && cargo install trunk
```

```sh
# backend
cd backend && cargo run -p webland-server

# frontend
cd frontend && trunk serve

# both
./scripts/dev.sh
```

The backend currently runs the **Phase 1** compositor: a winit-backed Wayland
compositor that renders mapped surfaces into a window on your existing desktop
(no browser yet). It binds its own `wayland-N` socket and logs the name. Point a
client at it, or have it spawn one:

```sh
# spawn a client automatically (any Wayland app)
WEBLAND_SPAWN=weston-terminal cargo run -p webland-server

# or connect one yourself to the socket it prints
cargo run -p webland-server        # logs e.g. display="wayland-2"
WAYLAND_DISPLAY=wayland-2 weston-terminal
```

### Phase 2/3: surfaces into the browser, and input back

Surfaces are encoded to H.264 on the GPU and streamed over a WebSocket; the
browser decodes them with WebCodecs and draws the result with WebGPU (2D canvas
where WebGPU is off), and sends pointer/keyboard input back. Frames are paced by
the browser, so clients redraw at its rate rather than into a growing queue.

A client that hands over a dmabuf never has its pixels copied: the buffer is
imported as `DRM_PRIME`, mapped to a VA-API surface and encoded from the memory
the client rendered into. `wl_shm` clients have no GPU buffer to import, so they
are uploaded instead, and fall back to a deflated damage rectangle — diffed
against what the browser already holds — if the encoder cannot take the surface
at all. Run headless — the browser is the only display:

```sh
# both halves; browser is the only display, with a client to show
WEBLAND_SPAWN=kitty ./scripts/dev.sh
# then open http://127.0.0.1:3030
```

Both dmabuf and `wl_shm` clients work (`kitty`, `weston-terminal`). Unset
`WEBLAND_HEADLESS` to also get a local winit window as a debugging ground-truth,
`WEBLAND_SIZE=WxH` changes the size clients are configured at,
`WEBLAND_RENDER_NODE` picks a different GPU, and `WEBLAND_BITRATE` sets the
encoder ceiling. If the zero-copy path is not being taken, `--example gpu_probe`
says whether EGL comes up on the render node at all.

Phase 2 is measured, not felt (`docs/roadmap.md`), so the wire cost has its own
client — it connects exactly as the browser does and reports the rate:

```sh
cargo run --release --manifest-path backend/Cargo.toml \
  -p webland-server --example measure -- 10
```

Measured with `kitty` at 1280x800, release build, all four Phase 2 gates met:

| | |
|---|---|
| Idle | one keyframe on connect, then nothing until something changes |
| Idle with a cursor | 2.2 frames/s, 5.0 KiB/s |
| Scrolling flat out | 39.8 frames/s, 588 KiB/s (~4.8 Mbit/s) |
| 1080p60, synthetic | 876 kbit/s at 255 fps encode (`--example encode_probe`) |

Frame rate tracks the load rather than a clock, which is the point of pacing on
browser acks. The bandwidth is H.264 doing the job deflate could not: the same
scrolling terminal cost ~47 Mbit/s as deflated damage rectangles.

Click-to-photon is measured in the browser and shown above the surface, since
both ends of it — the input and the frame it causes — happen there, so the page
clock is already the shared one. Typing currently lands around **52 ms median**.

Linux-first and Wayland-first. Xorg is not a target; X11 applications would be
handled through XWayland later, if at all.

