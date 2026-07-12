import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Tauri expects a fixed port and ignores vite's HMR websocket errors when the
// window is hidden. 1421 avoids clashing with easytier-gui's 1420.
const host = process.env.TAURI_DEV_HOST

export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 1421,
    strictPort: true,
    host: host || false,
    hmr: host
      ? { protocol: 'ws', host, port: 1422 }
      : undefined,
    watch: { ignored: ['**/src-tauri/**'] },
  },
  build: {
    target: 'es2021',
    minify: 'esbuild',
    sourcemap: false,
  },
})
