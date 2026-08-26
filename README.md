# Webland

Webland is an experimental Wayland-based Linux desktop environment whose
graphical display is presented through a web browser.

Linux applications talk Wayland to a Rust backend, which forwards surfaces and
input over the Webland protocol to a browser frontend that draws the desktop
with WebGPU.

**Status: architectural / prototyping stage.** The repository currently holds
the workspace layout, dependencies and tooling only — the compositor, the
protocol and the shell are not implemented.

## Layout

| Path | What lives here |
| --- | --- |
| `backend/` | Rust workspace: compositor, server, protocol, shared core |
| `frontend/` | TypeScript + Preact + Vite browser desktop |
| `shared/protocol/` | Language-neutral protocol definition |
| `docs/` | Architecture notes |
| `scripts/` | Development helpers |

## Development

```sh
# backend
cd backend && cargo run -p webland-server

# frontend
cd frontend && npm install && npm run dev

# both
./scripts/dev.sh
```

Linux-first and Wayland-first. Xorg is not a target; X11 applications would be
handled through XWayland later, if at all.

## Licence

MIT — see [LICENSE](LICENSE).
