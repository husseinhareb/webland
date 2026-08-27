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
| `gpu/` | WebGPU device and render pipelines |
| `protocol/` | Transport seam and codec |
| `wasm/` | Notes on hand-tuned WebAssembly beyond what Leptos already emits |

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
