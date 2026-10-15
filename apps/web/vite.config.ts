import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": "http://127.0.0.1:8080",
    },
  },
  preview: {
    port: 5193,
    proxy: {
      // E2E backend spawned by apps/web/e2e/helpers.ts (override with
      // PM_E2E_BACKEND when the backend lives elsewhere).
      "/api":
        process.env.PM_E2E_BACKEND ?? "http://127.0.0.1:18091",
    },
  },
});
