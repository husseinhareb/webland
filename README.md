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

Each `wl_shm` surface is captured, deflate-compressed, and streamed over a
WebSocket to the browser, which draws it to a canvas and sends pointer/keyboard
input back. Frames are paced by the browser (bounded queue) and only sent on
damage. Run headless — the browser is the only display:

```sh
# both halves; browser is the only display, with a client to show
WEBLAND_SPAWN=kitty ./scripts/dev.sh
# then open http://127.0.0.1:3000
```

Use an **shm** client (`kitty`, `weston-terminal`); GL/dmabuf-only clients have
no shm buffer to capture yet. Unset `WEBLAND_HEADLESS` to also get a local winit
window as a debugging ground-truth. Still a 2D-canvas/`Deflate` stopgap — WebGPU
and H.264/VA-API are the remaining rendering work.

Linux-first and Wayland-first. Xorg is not a target; X11 applications would be
handled through XWayland later, if at all.

