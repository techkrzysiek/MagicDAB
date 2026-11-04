import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/ws': { target: 'ws://127.0.0.1:8080', ws: true },
    },
  },
  build: {
    outDir: 'dist',
    chunkSizeWarningLimit: 1500,
    // stacks reported by the interface can be resolved (maps are not referenced)
    sourcemap: 'hidden',
  },
})
