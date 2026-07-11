/* @refresh reload */
import { render } from 'solid-js/web';
import Shell from './Shell';
import CaseView from './CaseView';
import './app.css';

const path = location.pathname.replace(/\/+$/, '');

function Root() {
  if (path === '/case') return <CaseView />;
  return <Shell />;
}

render(() => <Root />, document.getElementById('root')!);

// Register the PWA service worker in production builds only (dev uses Vite HMR,
// where a SW would fight the module reloads).
if ((import.meta as { env?: { PROD?: boolean } }).env?.PROD && 'serviceWorker' in navigator) {
  window.addEventListener('load', () => {
    void navigator.serviceWorker.register('/sw.js', { updateViaCache: 'none' }).catch(() => {});
  });
}
