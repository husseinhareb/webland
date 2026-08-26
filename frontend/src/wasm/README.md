# wasm

Reserved for performance-critical modules compiled to WebAssembly (decode,
damage tracking, protocol codec) once profiling justifies them.

Vite loads `.wasm` natively via `import init from "./mod.wasm?init"`, so no
bundler plugin is needed here.
