# Webland protocol

The single source of truth for messages exchanged between the Rust backend and
the browser frontend. The wire format is implemented once in the
`backend/crates/webland-protocol` crate; because the Leptos frontend also
compiles to WebAssembly, it depends on that same crate rather than reimplementing
the codec, so backend and frontend can never drift. `frontend/src/protocol` adds
only the browser-side transport seam over it. Neither side invents messages on
its own — this document specifies them.

Nothing is specified yet. Open questions, deliberately unanswered:

- encoding — a binary layout is expected, format undecided
- surface transfer — how pixel/GPU buffers reach the browser
- transport — WebSocket first, WebTransport later; the message set must not
  depend on either
