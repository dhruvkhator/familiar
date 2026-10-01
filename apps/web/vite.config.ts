import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss()],
  build: { outDir: "dist", sourcemap: false },
  // Out-of-the-way ports so Familiar never collides with other dev servers.
  server: { port: 47173, strictPort: true },
  preview: { port: 47173, strictPort: true },
});
