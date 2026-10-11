<div align="center">

# Webland

A Wayland compositor that uses a web browser as its screen. Linux applications run on one machine, and every window they open is streamed to the browser as a video of its own, where a desktop written in Rust and compiled to WebAssembly puts them together.

<img src="https://github.com/user-attachments/assets/1645f95e-6ff4-4d7e-a7eb-08059321e80d" alt="Webland in Chromium: nvim snapped to the left half of the screen, btop and vkcube floating on the right, and the panel along the bottom">

</div>

## Features

- **One stream per window**: each window is its own H.264 stream, encoded on the GPU with VA-API straight from the memory the application drew into, and decoded by the browser with WebCodecs
- **The desktop lives in the browser**: titlebars, a panel, a launcher, four workspaces, Alt+Tab, snapping and resizing are HTML and CSS, so moving, raising or hiding a window sends nothing over the network
- **Real applications**: GTK, Qt, Electron, terminals, Firefox, Vulkan programs, and X11 programs through XWayland, which Webland starts and manages itself
- **A session of its own**: a private D-Bus session bus, so single-instance applications open here rather than on the host desktop, with a system tray and desktop notifications served on that bus
- **Sound**: applications play into an audio sink that belongs to the session, streamed to the browser as Opus
- **Input that behaves**: clipboard in both directions, the cursor shape the application asks for, pointer lock for games, and the keyboard layout of whoever is sitting at the browser
- **Sharp on HiDPI screens**: the browser's `devicePixelRatio`, fractional values included, is the output scale
- **Paced by the browser**: a window gets its next frame only once the browser has drawn the last one, at the refresh rate the browser measured
- **Locked to a token**: every request, the WebSocket included, needs the session token

## How it works

```
Linux application
    |  Wayland, or X11 through XWayland
webland-compositor    smithay; captures each surface and encodes it with VA-API
    |
webland-server        hosts the compositor, the tray, notifications and audio
    |  WebSocket, carrying webland-protocol messages
frontend              Leptos in WebAssembly; WebCodecs decode, one canvas per window
    |
your screen
```

A remote desktop usually sends one picture of the whole screen. Webland sends each window separately and lets the browser do the compositing, which is what keeps the desktop cheap: the browser already holds every window, so dragging one, stacking them, minimizing and switching workspaces are all local changes and never touch the wire. Raising a window sends a few bytes, only because the compositor has a single seat and needs to know who holds the keyboard.

A client that hands over a dmabuf never has its pixels copied. The buffer is imported as `DRM_PRIME`, mapped to a VA-API surface and encoded right where the client rendered it. `wl_shm` clients have no GPU buffer to import, so their pixels are uploaded instead, and if the encoder cannot take a surface at all it falls back to deflated damage rectangles, diffed against what the browser already holds.

The browser sets the pace. A frame is acknowledged when it has actually been drawn, not when it comes off the socket, and the application is not told to draw its next one until that acknowledgement is back. When the page opens it times its own animation frames to measure the display's refresh rate, and the compositor ticks on that interval (60 Hz until it hears otherwise). Windows that are minimized or sitting on another workspace keep streaming, slowed to about two frames a second.

The browser's `devicePixelRatio` becomes the output scale, fractional values included. Applications that support `wp_fractional_scale` or integer buffer scales draw at the density of the screen in front of you. The rest, X11 programs among them, are drawn at their normal size and enlarged by the browser.

Both halves are Rust and link the same `webland-protocol` crate, so the wire format is defined once and the two ends cannot disagree about it.

## The desktop

Window chrome, stacking, the panel and the launcher are all drawn by the browser, so they cost the compositor nothing. Drag a window by its titlebar, or hold Alt and drag it from anywhere inside. Double-click a titlebar to maximize. Drop a window on the top edge of the screen to maximize it, or on the left or right edge to fill that half, and a preview shows where it will land before you let go.

<img src="https://github.com/user-attachments/assets/7afab2e8-b7cc-4658-ab7a-0c667004e82b" alt="btop being dragged to the right edge of the screen, with a translucent preview filling the right half where it will snap">

Maximizing and resizing are the two gestures the application itself has to answer: it is asked to redraw at the new size rather than being stretched. The corner grip sends that size once, when the drag ends, because every resize costs the client a reallocation and the stream a keyframe. Until the client answers, its last frame is stretched to fit, and it sharpens the moment the new one arrives.

There are four workspaces in the panel. A workspace is just a filter over windows the browser already has, so switching sends nothing. Drag a window onto a workspace button to send it there, or use the titlebar's right-click menu.

<p align="center">
  <img src="https://github.com/user-attachments/assets/cfc43cbc-6bad-40fe-97ab-1ad38fd00414" width="49%" alt="The titlebar's right-click menu on vkcube: minimize, maximize, workspace buttons 1 to 4, and close">
  <img src="https://github.com/user-attachments/assets/f89cd4bb-f134-4971-bd37-d961359bb036" width="49%" alt="The Alt+Tab switcher over a blurred desktop, with nvim, vkcube and btop">
</p>

Keys the desktop keeps for itself:

| Keys | Action |
|---|---|
| Alt+Tab | Switch windows |
| Alt+1 to Alt+4 | Go to a workspace |
| Alt+F4 or Alt+Shift+W | Close the focused window |

Everything else goes to the application. Browsers keep Ctrl+W, Ctrl+T, F11 and a few others for themselves, and the only way to get those back is the Keyboard Lock API, which Webland asks for when you go fullscreen. That works in Chromium over HTTPS or on localhost; in an ordinary tab those keys reach the browser instead of the application.

Webland draws one titlebar per window, never two. The compositor implements `xdg-decoration` and asks every client for server-side decorations. GTK applications draw their own headerbar regardless, so for them the shell leaves its titlebar off and turns the client's own move, maximize and minimize requests into the same gestures.

Menus, tooltips and dropdown lists are popups that the application places against its own window. They stream down the same path as windows, and the browser hangs each one off its parent, so a window dragged with a menu open takes the menu along. Clicking outside closes them.

<img src="https://github.com/user-attachments/assets/487035f7-40c2-49f3-b4aa-b3e5e236126f" alt="Firefox on workspace 2 showing the webland GitHub page, with its own right-click context menu open">

The pointer takes whatever shape the application asks for through `wp_cursor_shape_manager_v1`. Those shape names are CSS's own cursor names, so they go straight onto the canvas: an I-beam over text, a hand over a link, nothing at all where a game hides it. A client that grabs the pointer, like a game turning its camera, gets pointer lock in the browser and raw relative motion. While the lock is held the browser routes no events at all, so the shell keeps a virtual cursor, drawn from the host's cursor theme, and works out what is under it itself.

The clipboard works in both directions. Copying inside an application puts the text on the browser's clipboard, so it pastes anywhere on your machine. Pasting hands your clipboard to the application through the browser's own `paste` event, which is the one way a page gets the clipboard without a permission prompt. When you come back to the tab after copying something elsewhere, the clipboard is read once more, which the browser asks your permission for the first time. Only text crosses; images and file lists do not.

Window positions, workspaces and minimized state are kept for the tab, so reloading the page does not scatter the windows.

## The launcher

The button at the left of the panel opens a searchable list of everything installed, read from the system's `.desktop` files with their icons. Type to filter, use the arrow keys, and press Enter to start one.

<img src="https://github.com/user-attachments/assets/408d4948-f741-4837-a0e7-c4a40a00f9ac" alt="The launcher open over the desktop, listing installed applications with their icons and a search box at the top">

Webland starts a D-Bus session bus of its own, and everything it launches joins that bus instead of the host desktop's. This is what keeps the two sessions apart. Without it a single-instance application (Thunar, Obsidian, anything Electron) finds the copy already running on the host and hands it the window, so launching it here opened it over there. Keyring and polkit prompts start on Webland's bus too, so they show up in the browser and not on the host's screen.

What a bus cannot separate is state kept in a directory. Two Firefoxes cannot share one profile however they are started, so a second instance needs a profile of its own, and only you can decide where it lives. Give it one in `~/.config/webland/launch.conf`:

```
Firefox  = firefox --no-remote --profile ~/.webland/firefox
Chromium = chromium --user-data-dir=~/.webland/chromium
```

Create the directory first: Firefox will not make one whose parent is missing, and it says so in a dialog rather than on the terminal.

The name on the left does not have to be the entry's whole `Name=`. Any part of it works, and so does the `.desktop` file's own name, so `Firefox` finds the entry called `Firefox Web Browser`. Commands are split the way a shell would split them, so a path with spaces goes in quotes: `--profile "~/My Profiles"`.

## Tray, notifications and sound

The panel is a real `StatusNotifierItem` host on the session's bus. Applications that publish a tray icon (nm-applet, Steam, Electron apps) show up at the right end of the panel with their own icon, or their initial when the icon cannot be found. Left click runs the item's action and right click opens its menu, fetched fresh each time.

Webland is also the `org.freedesktop.Notifications` server on that bus. Notifications appear as toasts in the top right corner, and clicking one runs the application's default action, which is how a chat client knows to open the conversation the message was about.

<img src="https://github.com/user-attachments/assets/1239ee41-0e06-407b-8fd4-5a0cc1267b84" alt="A notification toast reading Build finished in the top right corner of the desktop">

Applications play into a null sink that belongs to the session, never into the host's speakers. Its output is encoded to Opus by `ffmpeg` and streamed to each connected browser, so the sound follows the desktop to whatever machine is watching it. Browsers do not let a page make sound before you interact with it, so audio starts with your first click or key press.

## Settings

The arrow next to the clock holds the settings that belong to the browser you are using rather than to any window.

<img src="https://github.com/user-attachments/assets/81f5e804-af95-41a4-8602-22005f57697d" alt="The settings panel open above the panel, with cursor lock, fullscreen, wallpaper, clock, network speed, notification and keyboard layout options">

- **Cursor lock** and **fullscreen**: Escape releases either
- **Wallpaper**: any image you pick is stored on the server, so every browser connected to the desktop sees it
- **24-hour clock** and **network speed**: the speed readout shows what the desktop is costing on the socket, down and up, right in the panel
- **Do not disturb** and how many seconds a notification stays
- **Keyboard layout and variant**, as xkb names (`us`, `fr`, `de` with `nodeadkeys`, and so on). The browser only reports which physical key was pressed, so the compositor needs to know the layout of the keyboard in front of you, which is not necessarily the host's. Changes apply immediately, and empty means the host's layout.

These are saved in the browser, so they are still yours tomorrow and in the next tab.

## Building and running

### Requirements

The host needs:

- Linux with a GPU that can encode H.264 through VA-API (Webland is developed on an AMD card with Mesa's driver)
- `ffmpeg`, for both the encoder libraries and the audio stream
- `Xwayland`, for X11 applications (without it, Wayland applications still work)
- `dbus-daemon`, for the session bus, the tray and notifications
- `pactl`, from PipeWire's PulseAudio layer or from PulseAudio, for sound (without it, applications play on the host's speakers)

The browser needs WebCodecs, which means a recent Chromium or Firefox, and a secure context: `localhost` or HTTPS. Webland is developed and tested in Chromium-based browsers.

The frontend compiles to WebAssembly, so building also takes the `wasm32-unknown-unknown` target, `trunk`, and a matching `wasm-bindgen`:

```sh
# Arch, with the distribution's rust package (no rustup)
sudo pacman -S rust-wasm trunk wasm-bindgen

# with rustup
rustup target add wasm32-unknown-unknown
cargo install trunk
```

### Running

`run.sh` drives both halves:

```sh
./run.sh            # backend and trunk serve, debug build
./run.sh release    # build both in release, then run them
./run.sh build      # release build only
./run.sh run        # run the release build without rebuilding
./run.sh check      # fmt, clippy and tests for both halves, plus a wasm build
```

The server prints a `http://127.0.0.1:3030/#token=...` link when it starts. Open it once: the page keeps the token as a cookie and removes it from the address bar, so later visits only need the plain address. The page reaches the protocol socket at `/ws` on its own origin, proxied to the backend, so only one port ever needs to be reachable.

The compositor opens its own `wayland-N` socket and prints the name. Start an application with the desktop, or point one at the socket yourself:

```sh
# start a client together with the desktop
WEBLAND_SPAWN=kitty ./run.sh

# or connect one to the socket the server printed
WAYLAND_DISPLAY=wayland-2 weston-terminal
```

If windows are arriving without the zero-copy path, `cargo run --manifest-path backend/Cargo.toml -p webland-compositor --example gpu_probe` shows whether EGL comes up on the render node and which dmabuf formats it can offer.

### Environment

| Variable | Default | Effect |
|---|---|---|
| `WEBLAND_HEADLESS` | `1` from `run.sh` | Unset it to also open a window on your current desktop that draws the surfaces directly, handy as ground truth |
| `WEBLAND_SPAWN` | none | Program to start with the compositor (a name or path, without arguments) |
| `WEBLAND_SIZE` | `1280x800` | Screen size until a browser connects and reports its own |
| `WEBLAND_PORT` | `3030` | Port the page is served on |
| `WEBLAND_WS` | `127.0.0.1:9001` | Address the protocol socket binds to |
| `WEBLAND_RENDER_NODE` | `/dev/dri/renderD128` | GPU to render and encode on |
| `WEBLAND_BITRATE` | 8 Mbit/s | Ceiling for the encoder |
| `WEBLAND_LAYOUT`, `WEBLAND_VARIANT` | host layout | Default xkb layout, until a browser picks its own in the settings |

## Using it from another machine

Every request has to carry the session token, kept in `$XDG_CONFIG_HOME/webland/token`. Delete that file to revoke access; the next start makes a new token. The cookie is `SameSite=Strict`, so another website cannot drive your desktop through your browser.

The traffic itself is plain HTTP, and `VideoDecoder` only exists in a secure context, so anywhere beyond localhost needs HTTPS in front. The page works out its socket address from wherever it is served, so any reverse proxy that forwards one port will do. On a tailnet:

```sh
sudo tailscale serve --bg --https=443 http://127.0.0.1:3030
```

Several browsers can be connected at once and they all see the same desktop.

## Performance

Measured on localhost with a release build in Chrome, on a Ryzen 7 5700 with a Radeon RX 6700 XT:

| Scenario | Result |
|---|---|
| Key press to drawn frame, kitty | 4 to 6 ms median |
| Idle terminal | about 0.9 kB/s down |
| kitty scrolling flat out, 1280x800 | 39.8 frames/s, 588 KiB/s (about 4.8 Mbit/s) |
| vkcube, 1168x560 window | 59 to 62 frames/s, 1.0 MB/s, server at 4 to 5% of a core |
| Four vkcube windows at once | 62.4 frames/s each, 4.0 MB/s in total, server at 10% of a core |
| vkcube at 2560x1440, encode only | 62.4 frames/s, about 1.0 MB/s |
| vkcube at 3840x2160, encode only | 61.1 frames/s, about 1.1 MB/s |

The vkcube rows were measured before the compositor started pacing to the browser's refresh rate; on a 60 Hz screen it now runs at exactly 60. The bandwidth is H.264 doing what deflate could not: the same scrolling terminal cost about 47 Mbit/s as deflated damage rectangles. The bitrate hardly moves with resolution, so at 4K it is the image quality that gives, not the frame rate. The 1440p and 4K rows had no browser attached, since a 1080p screen cannot show a viewport that big, so decoding at those sizes is still unmeasured.

Latency is measured in the page and shown above the desktop, because both the input and the frame it causes happen in the browser, whose clock is the one that matters. To measure the wire cost on your own machine, the `measure` example connects exactly as the browser does and reports frames and bytes per second:

```sh
cargo run --release --manifest-path backend/Cargo.toml \
  -p webland-server --example measure -- 10
```

## Project layout

| Path | What lives here |
|---|---|
| `backend/crates/webland-compositor` | The compositor: Wayland and XWayland, capture, the VA-API encoder, the launcher's application list |
| `backend/crates/webland-server` | The binary: event loop, WebSocket transport, token check, tray, notifications, audio, wallpaper |
| `backend/crates/webland-protocol` | The wire format, shared verbatim with the frontend |
| `backend/crates/webland-core` | Ids, geometry and errors used by both sides |
| `frontend/` | The desktop: Leptos compiled to WebAssembly with Trunk |
| `shared/protocol/` | The protocol described without reference to any language |

[docs/architecture.md](docs/architecture.md) explains how the pieces fit and why they were chosen.

## Status

Webland is a working desktop and still young. Things to know before relying on it:

- Applications without fractional scaling support, X11 ones included, are enlarged by the browser on HiDPI screens and look a little soft
- The clipboard carries text only
- Ctrl+W, Ctrl+T, F11 and similar keys go to the browser unless you are in fullscreen with Keyboard Lock
- On a link too slow for the stream the server drops the browser rather than queue frames forever, and the page says it is disconnected until you reload
- There are no packages yet; build it from source

## License

Webland is licensed under the [Apache License 2.0](LICENSE).
