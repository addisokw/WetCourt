import { createSignal, onCleanup, onMount, Show } from 'solid-js';

// The turret camera, reverse-proxied by the orchestrator at /vision/* (so it
// stays same-origin and works through the tunnel for remote operators). Plus the
// targeting controls: pick a body part, calibrate the boresight by clicking the
// feed, and ARM to let vision drive the turret (the orchestrator only relays aim
// while armed). No firing yet.
interface VisionState {
  ts: number;
  person?: boolean;
  frame?: { w: number; h: number };
  targets?: { chest?: number[] | null; head?: number[] | null; shoulders?: number[][] };
  eyes?: number[][] | null;
  target_part?: string;
  boresight?: number[];
  aim?: { pan: number; tilt: number };
  locked?: boolean;
  gains?: { pan: number; tilt: number; tolerance: number };
  fire_ok?: boolean;
  eye_clear?: boolean;
  head_confirm?: boolean;
}

const round3 = (n: number) => Math.round(n * 1000) / 1000;

const fmt = (p: number[]) => `${p[0]}, ${p[1]}`;
const PARTS = ['none', 'chest', 'head'] as const;

async function post(url: string, body: unknown) {
  await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}

export default function VisionPanel() {
  const [online, setOnline] = createSignal(false);
  const [state, setState] = createSignal<VisionState | null>(null);
  const [feedSrc, setFeedSrc] = createSignal('/vision/feed');
  const [armed, setArmed] = createSignal(false);
  const [boresightMode, setBoresightMode] = createSignal(false);
  // Local gain edits, or null to show the live value from the vision process.
  // (Falling back to state avoids the inputs going blank when the panel remounts
  // after a tab switch — the values reload from /state instead of needing reseed.)
  const [gainPan, setGainPan] = createSignal<number | null>(null);
  const [gainTilt, setGainTilt] = createSignal<number | null>(null);
  const [tol, setTol] = createSignal<number | null>(null);

  const panVal = () => gainPan() ?? state()?.gains?.pan;
  const tiltVal = () => gainTilt() ?? state()?.gains?.tilt;
  const tolVal = () => tol() ?? state()?.gains?.tolerance;

  let timer: number | undefined;

  async function poll() {
    const wasOnline = online();
    try {
      const res = await fetch('/vision/state');
      if (!res.ok) throw new Error(String(res.status));
      setState((await res.json()) as VisionState);
      setOnline(true);
      if (!wasOnline) setFeedSrc(`/vision/feed?t=${Date.now()}`);
    } catch {
      setOnline(false);
      setState(null);
    }
    try {
      const a = await fetch('/vision/arm');
      if (a.ok) setArmed(((await a.json()) as { armed: boolean }).armed);
    } catch {
      /* ignore */
    }
  }

  onMount(() => {
    void poll();
    timer = window.setInterval(poll, 700);
  });
  onCleanup(() => timer && clearInterval(timer));

  async function arm(on: boolean) {
    await post('/vision/arm', { armed: on });
    setArmed(on);
  }

  function onFeedClick(e: MouseEvent & { currentTarget: HTMLImageElement }) {
    if (!boresightMode()) return;
    const fr = state()?.frame;
    if (!fr) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const x = Math.round(((e.clientX - rect.left) / rect.width) * fr.w);
    const y = Math.round(((e.clientY - rect.top) / rect.height) * fr.h);
    void post('/vision/boresight', { x, y });
    setBoresightMode(false);
  }

  async function recenter() {
    await fetch('/vision/center', { method: 'POST' });
    setArmed(false);
  }

  async function setHeadConfirm(on: boolean) {
    await post('/vision/confirm_head', { enabled: on });
  }

  // Why the system would/wouldn't fire right now (display only this milestone).
  function fireStatus(): { ok: boolean; text: string } {
    const s = state();
    if (!s || (s.target_part ?? 'none') === 'none') return { ok: false, text: 'no target' };
    if (s.fire_ok) return { ok: true, text: 'FIRE OK' };
    if (!s.locked) return { ok: false, text: 'NO FIRE — not locked' };
    if (s.target_part === 'head' && !s.head_confirm) return { ok: false, text: 'NO FIRE — confirm head shot' };
    if (s.target_part === 'head' && s.eye_clear === false) return { ok: false, text: 'NO FIRE — eyes at risk' };
    return { ok: false, text: 'NO FIRE' };
  }

  function nudgeGain(axis: 'pan' | 'tilt', d: number) {
    const cur = (axis === 'pan' ? panVal() : tiltVal()) ?? 0;
    const v = round3(cur + d);
    if (axis === 'pan') setGainPan(v);
    else setGainTilt(v);
    void post('/vision/gains', { [`gain_${axis}`]: v });
  }
  function setGain(axis: 'pan' | 'tilt', v: number) {
    if (Number.isNaN(v)) return;
    if (axis === 'pan') setGainPan(v);
    else setGainTilt(v);
    void post('/vision/gains', { [`gain_${axis}`]: v });
  }
  function setTolerance(v: number) {
    if (Number.isNaN(v)) return;
    setTol(v);
    void post('/vision/gains', { tolerance: v });
  }

  const tgt = () => state()?.targets;
  const part = () => state()?.target_part ?? 'none';

  return (
    <div class="panel-card">
      <header class="panel-card-head">
        <h2>Turret vision</h2>
        <span class={`maint-indicator ${online() ? 'on' : 'off'}`}>
          <span class="dot" /> {online() ? 'live' : 'offline'}
        </span>
        <Show when={armed()}>
          <span class="vision-armed">ARMED</span>
        </Show>
      </header>

      <section class="panel-section">
        <div class={`vision-feed ${boresightMode() ? 'crosshair' : ''}`}>
          <img src={feedSrc()} alt="turret camera" onClick={onFeedClick} onError={() => setOnline(false)} />
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

      {/* Targeting */}
      <section class="panel-section">
        <h3>Targeting</h3>
        <div class="vision-row">
          <label>target</label>
          <div class="btn-row">
            {PARTS.map((p) => (
              <button class={`mini ${part() === p ? 'active' : ''}`} onClick={() => void post('/vision/target', { part: p })}>
                {p}
              </button>
            ))}
          </div>
        </div>

        <Show when={part() === 'head'}>
          <div class="vision-row">
            <label>head shot</label>
            <button class={`mini ${state()?.head_confirm ? 'active' : ''}`}
              onClick={() => void setHeadConfirm(!state()?.head_confirm)}>
              {state()?.head_confirm ? 'confirmed' : 'confirm head shot'}
            </button>
            <span class="muted small">head never fires unless confirmed + eyes clear</span>
          </div>
        </Show>

        <div class="vision-row">
          <label>boresight</label>
          <button class={`mini ${boresightMode() ? 'active' : ''}`} onClick={() => setBoresightMode(!boresightMode())}>
            {boresightMode() ? 'click the feed…' : 'set (click feed)'}
          </button>
          <span class="muted small">
            {state()?.boresight ? `at ${fmt(state()!.boresight!)}` : 'defaults to center'}
          </span>
        </div>

        <div class="vision-row">
          <label>arm</label>
          <Show
            when={armed()}
            fallback={<button class="arm-btn" onClick={() => void arm(true)}>Arm targeting</button>}
          >
            <button class="arm-btn armed" onClick={() => void arm(false)}>Disarm</button>
          </Show>
          <button class="mini" onClick={() => void recenter()}>Recenter</button>
          <span class="muted small">
            {armed() ? 'vision is driving the turret' : 'gun will not move until armed'}
          </span>
        </div>

        <div class="vision-row">
          <label>gain pan</label>
          <button class="mini" onClick={() => nudgeGain('pan', -0.005)}>−</button>
          <input class="gain-input" type="number" step="0.005" value={panVal() ?? ''}
            onChange={(e) => setGain('pan', parseFloat(e.currentTarget.value))} />
          <button class="mini" onClick={() => nudgeGain('pan', 0.005)}>+</button>
          <span class="muted small">deg/px — negative flips direction</span>
        </div>
        <div class="vision-row">
          <label>gain tilt</label>
          <button class="mini" onClick={() => nudgeGain('tilt', -0.005)}>−</button>
          <input class="gain-input" type="number" step="0.005" value={tiltVal() ?? ''}
            onChange={(e) => setGain('tilt', parseFloat(e.currentTarget.value))} />
          <button class="mini" onClick={() => nudgeGain('tilt', 0.005)}>+</button>
        </div>
        <div class="vision-row">
          <label>tolerance</label>
          <input class="gain-input" type="number" step="1" min="1" value={tolVal() ?? ''}
            onChange={(e) => setTolerance(parseInt(e.currentTarget.value, 10))} />
          <span class="muted small">px error for LOCKED</span>
        </div>

        <ul class="vision-state">
          <li>
            lock:{' '}
            <b class={state()?.locked ? 'locked' : ''}>{state()?.locked ? 'LOCKED' : 'tracking…'}</b>
          </li>
          <li>
            fire: <b class={fireStatus().ok ? 'locked' : 'nofire'}>{fireStatus().text}</b>
          </li>
          <Show when={state()?.aim}>
            <li class="muted small">aim {state()!.aim!.pan}°, {state()!.aim!.tilt}°</li>
          </Show>
        </ul>
        <p class="muted small">
          Tune the gains live until the target converges on the boresight without oscillating —
          if the gun runs <em>away</em> from the target, flip that axis's gain sign. Firing +
          eye-safety come next.
        </p>
      </section>

      {/* Raw detection */}
      <section class="panel-section">
        <h3>Detection</h3>
        <Show when={state()} fallback={<span class="muted small">no data</span>}>
          {(s) => (
            <ul class="vision-state">
              <li>person: <b>{s().person ? 'yes' : 'no'}</b></li>
              <Show when={tgt()?.chest}><li>chest: {fmt(tgt()!.chest!)}</li></Show>
              <Show when={tgt()?.head}><li>head: {fmt(tgt()!.head!)}</li></Show>
              <Show when={s().eyes}><li>eyes detected: {s().eyes!.length}</li></Show>
            </ul>
          )}
        </Show>
      </section>
    </div>
  );
}
