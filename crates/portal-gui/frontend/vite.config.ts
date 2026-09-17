import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue(), {
    name: 'tauri-motion-style-nonce',
    transformIndexHtml: {
      order: 'post',
      handler: () => [{ tag: 'style', attrs: { id: 'motion-csp' }, children: '/* Tauri supplies the per-document nonce for Motion runtime styles. */', injectTo: 'head' }],
    },
  }],
  base: './',
  server: { port: 4173, strictPort: true },
  build: { target: 'es2022' },
  test: { include: ['src/**/*.test.ts'], environment: 'node' },
})
