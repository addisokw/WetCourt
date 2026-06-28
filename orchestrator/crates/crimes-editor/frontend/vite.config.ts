import { defineConfig } from 'vite';
import solid from 'vite-plugin-solid';

export default defineConfig({
  plugins: [solid()],
  build: { outDir: 'dist', emptyOutDir: true },
  server: {
    port: 5174,
    // Dev: proxy the CRUD API to a locally-running `crimes-editor` binary.
    proxy: {
      '/api': { target: 'http://localhost:8080' },
    },
  },
});
