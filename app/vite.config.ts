import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  root: __dirname,
  envDir: __dirname,
  plugins: [react(), tailwindcss()],
  define: { global: "globalThis" },
  // Use the npm `buffer` polyfill, not Node's builtin (which Vite externalizes).
  resolve: { alias: { buffer: "buffer/" } },
  server: { port: 5173 },
});
