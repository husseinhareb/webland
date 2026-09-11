# Webland

A Wayland compositor that uses a browser as its display, streaming each window
separately so the browser does the compositing.

Linux applications talk Wayland to a Rust backend, which encodes each surface to
H.264 on the GPU and sends it over the Webland protocol to a browser frontend
that decodes it with WebCodecs and draws it on a 2D canvas, one canvas per
window. The frontend is written in Rust with Leptos and compiled to WebAssembly,
so it shares the protocol crate with the backend.

Streaming per surface rather than per screen is what makes the shell cheap: the
browser already holds every window, so moving, stacking and minimizing one are
local and send nothing at all.

**Status: the desktop runs.** Zero-copy dmabuf to VA-API to WebCodecs, a
browser-driven frame clock, and a shell with window chrome, a panel, a launcher
and workspaces. Notifications, menus and settings do not exist yet, and the
protocol is unauthenticated. See [docs/roadmap.md](docs/roadmap.md) for the
order of work.

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

The compositor binds its own `wayland-N` socket and logs the name. Without
`WEBLAND_HEADLESS` it also opens a winit window on your existing desktop, which
is useful as a visual ground truth. Point a client at it, or have it spawn one:

```sh
# spawn a client automatically (any Wayland app)
WEBLAND_SPAWN=weston-terminal cargo run -p webland-server

# or connect one yourself to the socket it prints
cargo run -p webland-server        # logs e.g. display="wayland-2"
WAYLAND_DISPLAY=wayland-2 weston-terminal
```

### Phase 2/3: surfaces into the browser, and input back

Surfaces are encoded to H.264 on the GPU and streamed over a WebSocket; the
browser decodes them with WebCodecs and draws the result on a 2D canvas, and
sends pointer, keyboard and wheel input back. Frames are paced by the browser,
so clients redraw at its rate rather than into a growing queue.

(There is a `wgpu` renderer in `frontend/src/gpu`, but nothing constructs it —
it is parked until the WebGPU path is worth switching on.)

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

### The shell

Window chrome, stacking, the panel and the launcher are drawn by the browser in
HTML and CSS, so they cost the compositor nothing. Dragging, raising and
minimizing a window cost no pixels at all. Raising one sends a few bytes, and
only because there is a single seat and the compositor has to know who is
holding it. Maximizing and resizing are the gestures the client itself must act
on: it is told to redraw at the new size rather than be stretched up from a
smaller one. The corner grip sends that size once, when the drag ends — every
configure costs the client a reallocation and the wire a keyframe, so the last
frame is stretched for the length of the gesture and sharpens when the client
answers.

The chrome is the only chrome: the compositor implements `xdg-decoration` and
answers every client `ServerSide`, so a client that would otherwise draw its own
titlebar does not put a second one, with a second set of buttons, inside the one
the shell drew. Clients that never ask — GTK does not implement the protocol at
all, and its headerbar is a widget rather than a decoration — get the opposite
treatment: the shell leaves its own titlebar off and forwards their move,
maximize and minimize requests to the browser, so the client's own bar drives
the same window management the shell's would have.

Menus, tooltips and combobox lists are popups: surfaces the client places
itself, against the window that opened it. They stream down the same path a
window does, and the browser hangs each one off its parent, so dragging a window
with a menu open drags the menu too. A click outside dismisses them, which is
what a pointer grab would do in a compositor that took one.

Workspaces are the clearest case of the architecture paying off: the browser
already holds every window, so a workspace is a filter over state it has, and
switching sends nothing at all. Four of them, in the panel; drag a window onto
one to send it there. Windows on a workspace you are not looking at are hidden
exactly as minimized ones are — and, like minimized ones, still streaming, which
is the encode cost to revisit if window counts grow.

The launcher lists what it finds in `.desktop` files, with their icons. An
application that will not start from a generic `Exec` line — anything that hands
off to a copy already running as the same user, such as Firefox — can be given a
different command in `~/.config/webland/launch.conf`:

```
Firefox  = firefox --new-instance
Chromium = chromium --user-data-dir=~/.webland/chromium
```

### Reaching it from another machine

The page derives its WebSocket URL from wherever it is served, so any reverse
proxy that forwards one port will do. It must be **HTTPS**: `VideoDecoder` is a
secure-context API, and over plain `http` to anything but localhost it is
`undefined` and every window stays black. On a tailnet:

```sh
sudo tailscale serve --bg --https=443 http://127.0.0.1:7681
```

Linux-first and Wayland-first. Xorg is not a target; X11 applications would be
handled through XWayland later, if at all.

