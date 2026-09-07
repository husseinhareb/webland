# Architecture

```
Linux applications
    ↓ Wayland
webland-compositor   (Rust, smithay)
    ↓
webland-server       (Rust, hosts the compositor, speaks the protocol)
    ↓ Webland protocol (WebSocket first, WebTransport later)
frontend             (Rust, Leptos → WebAssembly)
    ↓ WebGPU
user's display
```

## Crates

| Crate | Responsibility |
| --- | --- |
| `webland-core` | Shared vocabulary: ids, geometry, errors |
| `webland-protocol` | Wire format; no transport, no I/O |
| `webland-compositor` | Wayland compositor state and globals |
| `webland-server` | Binary: owns the event loop, the transport, process/PTY/clipboard integration |

## Frontend modules

| Module | Responsibility |
| --- | --- |
| `desktop/` | Shell UI: panels, dock, launcher, settings, notifications |
| `compositor/` | Places application surfaces in the desktop scene |
| `decode/` | WebCodecs H.264 decode, configured from the stream's own SPS |
| `gpu/` | WebGPU device and render pipelines |
| `input/` | Browser events to Wayland input, including modifier reconciliation |
| `latency/` | Click-to-photon timing, the number Phase 3 is judged on |
| `protocol/` | Transport seam and codec |

## Decisions

The reasoning behind these, and the order they get built in, is in
[roadmap.md](roadmap.md).

- **Wayland only.** Xorg is not a target. X11 clients would arrive via XWayland
  behind a feature flag, if ever.
- **Transport is replaceable.** The protocol is defined over framed binary
  messages; WebSocket is an implementation detail, not part of the contract.
- **The frontend is Rust + Leptos**, compiled to WebAssembly with Trunk. The
  whole frontend is WASM, so `webland-protocol` is a shared crate used verbatim
  on both sides and the wire format cannot drift. Leptos is chosen over a
  TypeScript framework for that single-sourcing, not for raw speed: the hot path
  (WebCodecs decode → WebGPU texture → composite) is browser-native and the same
  in any language. The cost is more `web-sys`/`wasm-bindgen` boilerplate around
  the newest browser APIs, accepted deliberately.
- **`smithay` for the compositor**, pulled in with default features off so the
  skeleton builds without DRM/libinput/udev system libraries. Backend features
  get enabled when a real backend is written.
- **Full keyboard access is opt-in, not required.** Browsers reserve `Ctrl+W`,
  `Ctrl+T`, `F11` and friends, and the only way to get them is the Keyboard Lock
  API, which needs fullscreen, HTTPS and Chromium. Webland does not require that:
  it runs in an ordinary tab, where the browser keeps the keys it reserves, and
  asks for Keyboard Lock only when the user goes fullscreen and the API exists.
  Requiring it would narrow the project to one browser in one mode to win a
  handful of shortcuts, which is the wrong trade for everything else that works
  in any tab. The cost is that those shortcuts reach the browser rather than the
  application until the user goes fullscreen, and that is worth saying out loud
  rather than treating as a bug.

  Modifier state is reconciled per event against `getModifierState` rather than
  tracked from keydowns alone, because the browser eating one keydown would
  otherwise leave a modifier stuck down for the client — the failure this
  decision makes more likely, and cheap to defend against.
