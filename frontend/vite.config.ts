import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [svelte()],
  
  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent vite from obscuring rust errors
  clearScreen: false,
  
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // 3. tell vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**", "../src-tauri/**"],
    },
  },
  
  // 4. to make use of `TAURI_DEBUG` and other env variables
  // https://tauri.studio/v1/api/config#buildconfig.beforedevcommand
  envPrefix: ['VITE_', 'TAURI_'],
  
  build: {
    // 5. Tauri supports es2021
    target: process.env.TAURI_PLATFORM == 'windows' ? 'chrome105' : 'safari13',
    // 6. don't minify for debug builds
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    // 7. produce sourcemaps for debug builds
    sourcemap: !!process.env.TAURI_DEBUG,
    // 8. output to project root for Tauri
    outDir: '../dist',
  },
  
  define: {
    __APP_VERSION__: JSON.stringify(process.env.npm_package_version),
  },
  
  optimizeDeps: {
    exclude: ['@tauri-apps/api'],
  },
})