import path from "path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    rollupOptions: {
      output: {
        // Everything used to land in one 880 kB chunk, which the webview has
        // to parse before it can draw anything. These three are the pieces
        // that are large, change rarely, and are worth caching separately;
        // the map is split further by being imported lazily in App.tsx.
        manualChunks: {
          // Named by package path rather than by bare specifier: React is
          // pulled in as `react/jsx-runtime` far more often than as `react`,
          // and listing the specifier alone produced an empty chunk.
          react: ["react", "react-dom", "react/jsx-runtime", "scheduler"],
          radix: [
            "@radix-ui/react-dialog",
            "@radix-ui/react-context-menu",
            "@radix-ui/react-dropdown-menu",
            "@radix-ui/react-select",
            "@radix-ui/react-tabs",
            "@radix-ui/react-tooltip",
          ],
          leaflet: ["leaflet", "react-leaflet", "react-leaflet-cluster", "leaflet.markercluster"],
        },
      },
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
        protocol: "ws",
        host,
        port: 1421,
      }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
