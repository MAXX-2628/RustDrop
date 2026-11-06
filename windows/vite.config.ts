import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // Design tokens live in ../shared/design.
  server: { port: 1420, strictPort: true, fs: { allow: [".."] } },
  build: { target: "es2022" },
});
