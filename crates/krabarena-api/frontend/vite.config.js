import { resolve } from "node:path";
import { defineConfig } from "vite";

const root = import.meta.dirname;
const isProduction = process.env.NODE_ENV === "production";

export default defineConfig({
  root,
  publicDir: false,
  build: {
    outDir: resolve(root, "../static/dist"),
    emptyOutDir: true,
    manifest: true,
    sourcemap: !isProduction,
    minify: isProduction,
    cssMinify: isProduction,
    rolldownOptions: {
      treeshake: {
        moduleSideEffects: false
      },
      input: {
        main: resolve(root, "src/main.js")
      },
      output: {
        entryFileNames: "assets/[name]-[hash].js",
        chunkFileNames: "assets/[name]-[hash].js",
        assetFileNames: "assets/[name]-[hash][extname]"
      }
    }
  }
});
