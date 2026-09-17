import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// 前端与后端同源托管：生产构建产物由 photos-api 静态服务直接提供，
// API 请求使用相对路径（/tasks、/config…），无需跨源与代理。
export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
