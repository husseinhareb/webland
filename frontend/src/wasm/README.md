# wasm

The whole frontend already compiles to WebAssembly through Leptos + Trunk, so
there is no JS/WASM boundary to cross for ordinary code.

This directory is reserved for the rare module that needs hand-tuning beyond
what the Leptos build emits (e.g. a hot decode or damage-tracking inner loop),
kept separate only if profiling ever justifies special build flags for it.
