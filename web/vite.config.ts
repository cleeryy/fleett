import { defineConfig } from "vite";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [tailwindcss()],
  resolve: {
    alias: {
      "@": new URL("./src", import.meta.url).pathname,
    },
  },
  server: {
    port: 5180,
    // Same-origin proxy so a real backend needs no CORS setup.
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8090",
        changeOrigin: false,
      },
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2022",
    // The shadcn stack (React 19 + Radix + sonner) lands just above the default
    // 500 kB warning for a single console bundle.
    chunkSizeWarningLimit: 700,
    rollupOptions: {
      onwarn(warning, defaultHandler) {
        // Radix and lucide ship "use client" markers for React Server Components.
        // They are meaningless in a Vite bundle.
        if (warning.code === "MODULE_LEVEL_DIRECTIVE") return;
        defaultHandler(warning);
      },
    },
  },
});
