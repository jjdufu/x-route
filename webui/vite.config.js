import { defineConfig } from "vite";

// 构建产物直接写入模块的 webroot（与 bindhosts / Tricky Addon 相同）
export default defineConfig({
  base: "./",
  build: {
    outDir: "../module/webroot",
    emptyOutDir: true,
    assetsInlineLimit: 0,
    chunkSizeWarningLimit: 2048,
  },
});
