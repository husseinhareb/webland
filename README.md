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

```sh
# backend
cd backend && cargo run -p webland-server

# frontend
cd frontend && trunk serve

# both
./scripts/dev.sh
```

Linux-first and Wayland-first. Xorg is not a target; X11 applications would be
handled through XWayland later, if at all.

