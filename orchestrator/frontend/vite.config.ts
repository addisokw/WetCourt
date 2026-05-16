import { defineConfig } from 'vite';
import solid from 'vite-plugin-solid';

export default defineConfig({
  plugins: [solid()],
  build: { outDir: 'dist', emptyOutDir: true },
  // TalkingHead resolves three via its own importmap by default; pre-bundle
  // so Vite emits ES modules instead of leaving the import-map dangling.
  optimizeDeps: { include: ['three', '@met4citizen/talkinghead'] },
  server: {
    port: 5173,
    proxy: {
      '/ws': { target: 'ws://localhost:8080', ws: true },
      '/operator': { target: 'http://localhost:8080' },
    },
  },
});
