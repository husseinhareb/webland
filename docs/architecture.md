# Architecture

```
Linux applications
    ↓ Wayland
webland-compositor   (Rust, smithay)
    ↓
webland-server       (Rust, hosts the compositor, speaks the protocol)
    ↓ Webland protocol (WebSocket first, WebTransport later)
frontend             (TypeScript, Preact)
    ↓ WebGPU / WebAssembly
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
| `wasm/` | Future performance-critical modules |

## Decisions

- **Wayland only.** Xorg is not a target. X11 clients would arrive via XWayland
  behind a feature flag, if ever.
- **Transport is replaceable.** The protocol is defined over framed binary
  messages; WebSocket is an implementation detail, not part of the contract.
- **The frontend is TypeScript.** WebAssembly is reserved for parts that
  profiling shows need it, rather than being the starting point.
- **`smithay` for the compositor**, pulled in with default features off so the
  skeleton builds without DRM/libinput/udev system libraries. Backend features
  get enabled when a real backend is written.
