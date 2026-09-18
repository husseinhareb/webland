# Webland

A Wayland compositor that uses a browser as its display. Every window is
streamed as its own video and the browser composites them.

Linux applications talk Wayland to a Rust backend. The backend encodes each
surface to H.264 on the GPU and sends it over a WebSocket to a frontend written
in Rust with Leptos and compiled to WebAssembly. The frontend decodes with
WebCodecs and draws every window into its own 2D canvas. Both halves link the
same protocol crate, so the wire format cannot drift.

Streaming per surface instead of per screen is what keeps the shell cheap. The
browser already holds every window, so moving, stacking, minimizing and
switching workspaces happen locally and send nothing at all.

## Status

The desktop runs. Zero-copy dmabuf to VA-API to WebCodecs, a frame clock driven
by the browser, and a shell with window chrome, a panel, a launcher, four
workspaces, Alt+Tab, edge snapping and a shared clipboard. X11 applications work
through XWayland, which the compositor starts and manages itself.

Not there yet: application notifications (no D-Bus daemon), a settings UI, and
authentication. The protocol has no auth at all, so bind it to localhost and put
a proxy in front if you want it elsewhere. [docs/roadmap.md](docs/roadmap.md)
has the order of work, [docs/architecture.md](docs/architecture.md) the shape of
the code.

## Layout

| Path | What lives here |
| --- | --- |
| `backend/` | Rust workspace: compositor, server, protocol, shared core |
| `frontend/` | Rust and Leptos browser desktop, built to WebAssembly with Trunk |
| `shared/protocol/` | Language-neutral protocol definition |
| `docs/` | Architecture notes and roadmap |

## Building and running

The frontend compiles to WebAssembly, so you need the `wasm32-unknown-unknown`
target plus `trunk` and a matching `wasm-bindgen`.

```sh
# Arch (distro rust, no rustup): versions stay locked to the `rust` package
sudo pacman -S rust-wasm trunk wasm-bindgen

# rustup toolchains
rustup target add wasm32-unknown-unknown && cargo install trunk
```

`.run.sh` drives both halves:

```sh
./.run.sh            # dev: backend + trunk serve, debug build
./.run.sh release    # build both in release, then run them
./.run.sh build      # release build only
./.run.sh run        # run release artifacts without rebuilding
./.run.sh check      # fmt, clippy and tests for both halves, plus a wasm build
```

Then open http://127.0.0.1:3030. The page serves the protocol socket from its
own origin at `/ws`, proxied to the backend, so only one port ever has to be
reachable.

The compositor binds its own `wayland-N` socket and logs the name. Point a
client at it, or have it spawn one:

```sh
# spawn a client with the desktop
WEBLAND_SPAWN=kitty ./.run.sh

# or connect one yourself to the socket it prints
cargo run --manifest-path backend/Cargo.toml -p webland-server   # logs display="wayland-2"
WAYLAND_DISPLAY=wayland-2 weston-terminal
```

Both dmabuf and `wl_shm` clients work. A client that hands over a dmabuf never
has its pixels copied: the buffer is imported as `DRM_PRIME`, mapped to a VA-API
surface and encoded straight out of the memory the client rendered into.
`wl_shm` clients have no GPU buffer to import, so they get uploaded instead, and
fall back to a deflated damage rectangle diffed against what the browser already
holds if the encoder cannot take the surface at all.

If the zero-copy path is not being taken, `--example gpu_probe` says whether EGL
comes up on the render node at all.

### Environment

| Variable | Default | Effect |
| --- | --- | --- |
| `WEBLAND_HEADLESS` | `1` from `.run.sh` | Unset it to also open a winit window on your existing desktop, useful as visual ground truth |
| `WEBLAND_SPAWN` | none | Client to start with the compositor |
| `WEBLAND_SIZE` | `1280x800` | Size clients are configured at |
| `WEBLAND_PORT` | `3030` | Port the page is served on |
| `WEBLAND_WS` | `127.0.0.1:9001` | Address the protocol socket binds to |
| `WEBLAND_RENDER_NODE` | `/dev/dri/renderD128` | GPU to encode on |
| `WEBLAND_BITRATE` | 8 Mbit/s | Encoder ceiling for the worst case |
| `WEBLAND_LAYOUT`, `WEBLAND_VARIANT` | host layout | xkb keyboard layout, since the browser only reports physical key positions |

## The shell

Window chrome, stacking, the panel and the launcher are drawn by the browser in
HTML and CSS, so they cost the compositor nothing. Dragging, raising and
minimizing a window send no pixels. Raising one sends a few bytes, and only
because there is a single seat and the compositor has to know who holds it.

Maximizing and resizing are the gestures the client itself has to act on: it is
told to redraw at the new size rather than stretched up from a smaller one. The
corner grip sends that size once, when the drag ends, because every configure
costs the client a reallocation and the wire a keyframe. The last frame stays
stretched for the length of the gesture and sharpens when the client answers.

The shell's chrome is the only chrome. The compositor implements
`xdg-decoration` and answers every client `ServerSide`, so nothing draws a second
titlebar inside the first. Clients that never ask get the opposite treatment:
GTK does not implement the protocol at all and its headerbar is a widget rather
than a decoration, so the shell leaves its own titlebar off and forwards the
client's move, maximize and minimize requests to the browser instead.

Menus, tooltips and combobox lists are popups: surfaces the client places itself
against the window that opened it. They stream down the same path a window does
and the browser hangs each one off its parent, so dragging a window with a menu
open drags the menu with it. A click outside dismisses them.

Workspaces are where the architecture pays off most clearly. The browser already
holds every window, so a workspace is a filter over state it has and switching
sends nothing at all. There are four of them in the panel, and dragging a window
onto one sends it there. Windows on a workspace you are not looking at are
hidden the same way minimized ones are, and like minimized ones they keep
streaming. That is the encode cost to revisit if window counts grow.

Keys the shell keeps for itself: Alt+Tab to switch windows, Alt+1 to Alt+4 for
workspaces, Alt+F4 or Alt+Shift+W to close. Everything else goes to the client.
Browsers reserve Ctrl+W, Ctrl+T, F11 and friends, and the only way to get them
back is the Keyboard Lock API, which needs fullscreen and HTTPS on Chromium.

The clipboard crosses both ways. A copy inside a client is read out of its
selection and put on the browser's clipboard, so it pastes anywhere on the
machine. A paste hands the browser's clipboard back, carried by the browser's
own `paste` event, which is the one way a page is given the clipboard without a
permission prompt. Text only: an image or a file list is a copy the browser
cannot take.

The pointer is the client's to name. `wp_cursor_shape_manager_v1` gets a shape
by name and those names are CSS's names, so what the client asked for goes
straight onto the canvas: an I-beam over text, a hand over a link, nothing at
all where a client hides it. That applies over client pixels only; the shell's
chrome keeps the cursors its stylesheet gives it.

A client that grabs the pointer, like a game moving a camera, gets pointer lock
in the browser and relative motion straight through. While the lock is held the
browser routes no events, so the shell tracks a virtual cursor and hit-tests its
own chrome itself.

## The launcher

It lists what it finds in `.desktop` files, with their icons. An application
that will not start from a generic `Exec` line, such as anything that hands off
to a copy already running as the same user, can be given a different command in
`~/.config/webland/launch.conf`:

```
Firefox  = firefox --new-instance
Chromium = chromium --user-data-dir=~/.webland/chromium
```

The name on the left need not be the entry's whole `Name=`. Any part of it will
do, as will the `.desktop` file's own name, so `Firefox` finds the entry that
calls itself `Firefox Web Browser`. Commands are quoted as a shell would quote
them, so a path with a space in it goes in quotes: `--profile "~/My Profiles"`.

## Reaching it from another machine

The page derives its socket URL from wherever it is served, so any reverse proxy
that forwards one port will do. It has to be HTTPS: `VideoDecoder` is a
secure-context API, and over plain `http` to anything but localhost it is
`undefined` and every window stays black. On a tailnet:

```sh
sudo tailscale serve --bg --https=443 http://127.0.0.1:3030
```

## Performance

The wire cost has its own client, which connects exactly as the browser does and
reports the rate:

```sh
cargo run --release --manifest-path backend/Cargo.toml \
  -p webland-server --example measure -- 10
```

Measured with `kitty` at 1280x800, release build:

| Scenario | Result |
|---|---|
| Idle | one keyframe on connect, then nothing until something changes |
| Idle with a cursor | 2.2 frames/s, 5.0 KiB/s |
| Scrolling flat out | 39.8 frames/s, 588 KiB/s (about 4.8 Mbit/s) |
| 1080p60, synthetic | 876 kbit/s at 255 fps encode (`--example encode_probe`) |

Frame rate tracks the load rather than a clock, which is the point of pacing on
browser acks. The bandwidth is H.264 doing the job deflate could not: the same
scrolling terminal cost about 47 Mbit/s as deflated damage rectangles.

Click-to-photon is measured in the browser and shown above the surface, since
the input and the frame it causes both happen there and the page clock is
already the shared one. Typing lands around 52 ms median.
