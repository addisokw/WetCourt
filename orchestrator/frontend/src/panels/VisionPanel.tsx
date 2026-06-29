import { createSignal, onCleanup, onMount, Show } from 'solid-js';

// The turret camera, reverse-proxied by the orchestrator at /vision/* (so it
// stays same-origin and works through the tunnel for remote operators). This
// milestone only *views* the feed + detection — no aiming or firing.
interface VisionState {
  ts: number;
  person?: boolean;
  frame?: { w: number; h: number };
  targets?: { chest?: number[] | null; head?: number[] | null; shoulders?: number[][] };
  eyes?: number[][] | null;
}

const fmt = (p: number[]) => `${p[0]}, ${p[1]}`;

export default function VisionPanel() {
  const [online, setOnline] = createSignal(false);
  const [state, setState] = createSignal<VisionState | null>(null);
  const [feedSrc, setFeedSrc] = createSignal('/vision/feed');

  let timer: number | undefined;

  async function poll() {
    const wasOnline = online();
    try {
      const res = await fetch('/vision/state');
      if (!res.ok) throw new Error(String(res.status));
      setState((await res.json()) as VisionState);
      setOnline(true);
      // Feed <img> won't auto-recover after the vision process restarts, so
      // re-request it on the offline→online transition.
      if (!wasOnline) setFeedSrc(`/vision/feed?t=${Date.now()}`);
    } catch {
      setOnline(false);
      setState(null);
    }
  }

  onMount(() => {
    void poll();
    timer = window.setInterval(poll, 1000);
  });
  onCleanup(() => timer && clearInterval(timer));

  const tgt = () => state()?.targets;

  return (
    <div class="panel-card">
      <header class="panel-card-head">
        <h2>Turret vision</h2>
        <span class={`maint-indicator ${online() ? 'on' : 'off'}`}>
          <span class="dot" /> {online() ? 'live' : 'offline'}
        </span>
      </header>

      <section class="panel-section">
        <div class="vision-feed">
          <img src={feedSrc()} alt="turret camera" onError={() => setOnline(false)} />
          <Show when={!online()}>
            <div class="vision-offline">
              <p>vision process offline</p>
              <p class="muted small">
                start it on the booth PC: <code>cd vision &amp;&amp; uv run vision.py</code>
              </p>
              <button onClick={() => void poll()}>Retry</button>
            </div>
          </Show>
        </div>
      </section>

      <section class="panel-section">
        <h3>Detection</h3>
        <Show when={state()} fallback={<span class="muted small">no data</span>}>
          {(s) => (
            <ul class="vision-state">
              <li>person: <b>{s().person ? 'yes' : 'no'}</b></li>
              <Show when={s().frame}>
                <li class="muted small">frame {s().frame!.w}×{s().frame!.h}</li>
              </Show>
              <Show when={tgt()?.chest}><li>chest: {fmt(tgt()!.chest!)}</li></Show>
              <Show when={tgt()?.head}><li>head: {fmt(tgt()!.head!)}</li></Show>
              <Show when={s().eyes}><li>eyes detected: {s().eyes!.length}</li></Show>
            </ul>
          )}
        </Show>
        <p class="muted small">Sensing only — aiming and firing come in later milestones.</p>
      </section>
    </div>
  );
}
