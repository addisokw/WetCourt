import { defineConfig } from 'vite';
import solid from 'vite-plugin-solid';

export default defineConfig({
  plugins: [solid()],
  build: { outDir: 'dist', emptyOutDir: true },
  server: {
    port: 5173,
    proxy: {
      '/ws': { target: 'ws://localhost:8080', ws: true },
      '/operator': { target: 'http://localhost:8080' },
      '/maintenance': { target: 'http://localhost:8080' },
      '/vision': { target: 'http://localhost:8080' },
    },
  },
});
