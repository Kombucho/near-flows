import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig(({ command }) => ({
  // Project Pages serves this repo at /near-flows/. Dev stays at /.
  base: command === "build" ? "/near-flows/" : "/",
  plugins: [react()],
  server: { host: "127.0.0.1", port: 5173 },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
}));
