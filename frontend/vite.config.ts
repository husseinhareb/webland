import preact from "@preact/preset-vite";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [preact()],
  // Vite handles `.wasm?init` natively; no plugin needed until we ship a module.
  server: { port: 3000 },
});
