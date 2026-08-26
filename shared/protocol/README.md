# Webland protocol

The single source of truth for messages exchanged between the Rust backend and
the browser frontend. Both `backend/crates/webland-protocol` and
`frontend/src/protocol` implement whatever is specified here; neither invents
messages on its own.

Nothing is specified yet. Open questions, deliberately unanswered:

- encoding — a binary layout is expected, format undecided
- surface transfer — how pixel/GPU buffers reach the browser
- transport — WebSocket first, WebTransport later; the message set must not
  depend on either
