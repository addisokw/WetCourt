import { createMemo, createSignal, onCleanup, onMount, Show } from 'solid-js';
import {
  calibrations,
  fetchCalibrations,
  maintenanceActive,
  saveCalibration,
  sendCommand,
  updateCalibration,
  VisionCal,
} from '../maintenance';
import { AckChip, useAck } from './common';
import VisionFeed from './VisionFeed';

// The turret camera feed (see VisionFeed) plus the targeting controls: pick a
// body part, calibrate the boresight by clicking the feed, and ARM to let
// vision drive the turret (the orchestrator only relays aim while armed).
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
  tracks?: { id: number; center: number[]; box: number[] }[];
  selected?: number | null;
  selected_visible?: boolean;
}

/** What a click on the feed does. */
type ClickMode = 'off' | 'aim' | 'select' | 'boresight';

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
  const [armed, setArmed] = createSignal(false);
  const [clickMode, setClickMode] = createSignal<ClickMode>('off');
  // Auto-fire: the gun fires once vision holds a lock for `dwell`. `dwellLocal`
  // holds an in-progress edit; `afStatus` mirrors the server (dwell + how long
  // the current lock has held). `fireAck` tracks the manual Fire-now button.
  const [autoFire, setAutoFire] = createSignal(false);
  const [dwellLocal, setDwellLocal] = createSignal<number | null>(null);
  const [afStatus, setAfStatus] = createSignal<{ dwell_ms: number; locked_ms: number }>({ dwell_ms: 2000, locked_ms: 0 });
  const [fireAck, runFire] = useAck();
  const dwellMs = () => dwellLocal() ?? afStatus().dwell_ms;
  const fireMs = () => calibrations().squirt?.fire_ms ?? 150;
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
    try {
      const res = await fetch('/vision/state');
      if (!res.ok) throw new Error(String(res.status));
      setState((await res.json()) as VisionState);
      setOnline(true);
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
    try {
      const f = await fetch('/vision/autofire');
      if (f.ok) {
        const j = (await f.json()) as { enabled: boolean; dwell_ms: number; locked_ms: number };
        setAutoFire(j.enabled);
        setAfStatus({ dwell_ms: j.dwell_ms, locked_ms: j.locked_ms });
      }
    } catch {
      /* ignore */
    }
  }

  onMount(() => {
    void poll();
    void fetchCalibrations().catch(() => {}); // for the squirt fire duration
    timer = window.setInterval(poll, 700);
  });
  onCleanup(() => {
    if (timer) clearInterval(timer);
  });

  async function arm(on: boolean) {
    await post('/vision/arm', { armed: on });
    setArmed(on);
  }

  async function enableAutoFire(on: boolean) {
    await post('/vision/autofire', { enabled: on });
    setAutoFire(on);
  }
  async function setDwellSeconds(sec: number) {
    if (Number.isNaN(sec)) return;
    const ms = Math.max(0, Math.round(sec * 1000));
    setDwellLocal(ms);
    await post('/vision/autofire', { dwell_ms: ms });
  }
  // Manual immediate shot for the squirt's configured duration.
  function fireNow() {
    void runFire(sendCommand('squirt', { cmd: 'fire', ms: fireMs() }));
  }

  function onFeedClick(e: MouseEvent & { currentTarget: HTMLImageElement }) {
    const mode = clickMode();
    if (mode === 'off') return;
    const fr = state()?.frame;
    if (!fr) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const x = Math.round(((e.clientX - rect.left) / rect.width) * fr.w);
    const y = Math.round(((e.clientY - rect.top) / rect.height) * fr.h);
    if (mode === 'boresight') {
      void post('/vision/boresight', { x, y });
      setClickMode('off'); // boresight is a one-shot calibration
    } else if (mode === 'aim') {
      // One-shot nudge toward the click; stays in aim mode for refinement clicks.
      void post('/vision/aimpoint', { x, y });
    } else {
      void post('/vision/select', { x, y }).then(() => void poll());
    }
  }

  async function recenter() {
    await fetch('/vision/center', { method: 'POST' });
    setArmed(false);
  }

  // Why the system would/wouldn't fire right now (display only).
  function fireStatus(): { ok: boolean; text: string } {
    const s = state();
    if (!s || (s.target_part ?? 'none') === 'none') return { ok: false, text: 'no target' };
    if (s.fire_ok) return { ok: true, text: 'FIRE OK' };
    if (!s.locked) return { ok: false, text: 'NO FIRE — not locked' };
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

  // --- Tuning persistence (mirrors the device panels' Apply/Save flow). Live
  // edits above take effect immediately in the vision process but die with it;
  // Save writes them into calibration/vision.toml, and the orchestrator
  // re-seeds the vision process from there whenever it (re)connects.
  const savedTuning = createMemo<VisionCal | null>(() => calibrations().vision?.vision ?? null);
  const [tuneStatus, setTuneStatus] = createSignal('');
  const [tuneError, setTuneError] = createSignal('');

  // Vision-failure fallback aim (fixed turret pan/tilt°, above the mic). Not a
  // live vision value — it lives only in the saved tuning — so it's a local
  // form seeded from the save: null field = show saved, NaN = cleared.
  const [fbPan, setFbPan] = createSignal<number | null>(null);
  const [fbTilt, setFbTilt] = createSignal<number | null>(null);
  const fbPanVal = () => fbPan() ?? savedTuning()?.fallback_aim?.[0] ?? NaN;
  const fbTiltVal = () => fbTilt() ?? savedTuning()?.fallback_aim?.[1] ?? NaN;
  /** The fallback as Save would persist it: both axes set, else none. */
  const fbLive = (): [number, number] | null =>
    Number.isFinite(fbPanVal()) && Number.isFinite(fbTiltVal())
      ? [fbPanVal(), fbTiltVal()]
      : null;
  // Point the turret (and the judge's gaze) at the fallback to sight it in —
  // degree-based AIM through the maintenance gate, like the Turret panel.
  function aimAtFallback() {
    const fb = fbLive();
    if (!fb) return;
    void sendCommand('turret', { cmd: 'aim', pan: fb[0], tilt: fb[1] }, true);
    void sendCommand('judge_neck', { cmd: 'aim', pan: fb[0], tilt: fb[1] }, true);
  }

  /** The tuning as currently live (what Save would persist). */
  function liveTuning(): VisionCal | null {
    const p = panVal();
    const t = tiltVal();
    const tolv = tolVal();
    if (p == null || t == null || tolv == null) return null; // vision offline, nothing to capture
    const bs = state()?.boresight;
    // "none" is a transient console state, never a trial target — keep the
    // saved part (or the head default) in that case.
    const livePart = part();
    const target_part =
      livePart === 'chest' || livePart === 'head'
        ? livePart
        : savedTuning()?.target_part ?? 'head';
    return {
      gain_pan: p,
      gain_tilt: t,
      tolerance: tolv,
      boresight:
        bs && bs.length === 2
          ? [Math.round(bs[0]), Math.round(bs[1])]
          : savedTuning()?.boresight ?? null,
      target_part,
      autofire_dwell_ms: dwellMs(),
      fallback_aim: fbLive(),
    };
  }

  const tuningDirty = createMemo<boolean>(() => {
    const live = liveTuning();
    const saved = savedTuning();
    if (!live) return false;
    if (!saved) return true;
    return (
      round3(live.gain_pan) !== round3(saved.gain_pan) ||
      round3(live.gain_tilt) !== round3(saved.gain_tilt) ||
      live.tolerance !== saved.tolerance ||
      live.target_part !== saved.target_part ||
      String(live.boresight ?? '') !== String(saved.boresight ?? '') ||
      live.autofire_dwell_ms !== (saved.autofire_dwell_ms ?? live.autofire_dwell_ms) ||
      String(live.fallback_aim ?? '') !== String(saved.fallback_aim ?? '')
    );
  });

  async function saveTuning() {
    const live = liveTuning();
    if (!live) return;
    setTuneError('');
    setTuneStatus('saving…');
    try {
      const base = calibrations().vision ?? { role: 'vision' };
      await updateCalibration('vision', { ...base, vision: live });
      await saveCalibration('vision');
      setFbPan(null); // re-sync the fallback inputs to the now-saved value
      setFbTilt(null);
      setTuneStatus('saved to disk');
    } catch (e) {
      setTuneError(String(e));
      setTuneStatus('');
    }
  }

  /** Push the saved tuning back into the live vision process. */
  async function reapplySaved() {
    const saved = savedTuning();
    if (!saved) return;
    setTuneError('');
    setTuneStatus('re-applying…');
    try {
      await post('/vision/gains', {
        gain_pan: saved.gain_pan,
        gain_tilt: saved.gain_tilt,
        tolerance: saved.tolerance,
      });
      if (saved.boresight) {
        await post('/vision/boresight', { x: saved.boresight[0], y: saved.boresight[1] });
      }
      await post('/vision/target', { part: saved.target_part });
      if (saved.autofire_dwell_ms != null) {
        await post('/vision/autofire', { dwell_ms: saved.autofire_dwell_ms });
        setDwellLocal(null);
      }
      setGainPan(null);
      setGainTilt(null);
      setTol(null);
      await poll();
      setTuneStatus('saved tuning re-applied');
    } catch (e) {
      setTuneError(String(e));
      setTuneStatus('');
    }
  }

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
        <VisionFeed online={online()} class={clickMode() !== 'off' ? 'crosshair' : ''} onFeedClick={onFeedClick}>
          <p>vision process offline</p>
          <p class="muted small">
            start it on the booth PC: <code>cd vision &amp;&amp; uv run vision.py</code>
          </p>
          <button onClick={() => void poll()}>Retry</button>
        </VisionFeed>
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

        <div class="vision-row">
          <label>feed click</label>
          <div class="btn-row">
            {(['off', 'aim', 'select', 'boresight'] as const).map((m) => (
              <button
                class={`mini ${clickMode() === m ? 'active' : ''}`}
                onClick={() => setClickMode(clickMode() === m ? 'off' : m)}
              >
                {m === 'aim' ? 'aim here' : m === 'select' ? 'select person' : m === 'boresight' ? 'set boresight' : 'off'}
              </button>
            ))}
          </div>
          <span class="muted small">
            {clickMode() === 'aim'
              ? armed()
                ? 'click the feed to point the gun there (click again to refine)'
                : '⚠ arm targeting — clicks aim, but the gun won’t move until armed'
              : clickMode() === 'select'
                ? 'click a person’s box to track them'
                : clickMode() === 'boresight'
                  ? 'click where the gun actually points'
                  : state()?.boresight
                    ? `boresight at ${fmt(state()!.boresight!)}`
                    : 'boresight defaults to center'}
          </span>
        </div>

        <Show when={state()?.selected != null || (state()?.tracks?.length ?? 0) > 0}>
          <div class="vision-row">
            <label>tracking</label>
            <Show
              when={state()?.selected != null}
              fallback={
                <span class="muted small">
                  {state()!.tracks!.length} {state()!.tracks!.length === 1 ? 'person' : 'people'} in frame — nearest to boresight is targeted
                </span>
              }
            >
              <span class={state()?.selected_visible ? 'sel-badge' : 'sel-badge lost'}>
                #{state()!.selected} {state()?.selected_visible ? 'selected' : 'LOST — gun holding'}
              </span>
              <button class="mini" onClick={() => void post('/vision/select', { clear: true }).then(() => void poll())}>
                clear
              </button>
            </Show>
          </div>
        </Show>

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
          <label>fire</label>
          <button class="mini" onClick={fireNow}>Fire now ({fireMs()} ms)</button>
          <AckChip ack={fireAck()} />
          <span class="muted small">manual shot for the squirt's set duration</span>
        </div>

        <div class="vision-row">
          <label>auto-fire</label>
          <Show
            when={autoFire()}
            fallback={
              <button
                class="mini"
                disabled={!maintenanceActive()}
                title={maintenanceActive() ? undefined : 'requires maintenance mode'}
                onClick={() => void enableAutoFire(true)}
              >
                Enable
              </button>
            }
          >
            <button class="mini active" onClick={() => void enableAutoFire(false)}>Disable</button>
          </Show>
          <label class="af-dwell">
            after
            <input
              class="gain-input"
              type="number"
              step="0.5"
              min="0"
              value={(dwellMs() / 1000).toFixed(1)}
              onChange={(e) => void setDwellSeconds(parseFloat(e.currentTarget.value))}
            />
            s locked
          </label>
          <span class="muted small">
            {maintenanceActive()
              ? 'fires once each time the lock holds this long'
              : '🔒 tuning tool — enabling requires maintenance mode'}
          </span>
        </div>

        <Show when={autoFire()}>
          <div class="vision-row">
            <label />
            <Show
              when={armed()}
              fallback={<span class="af-warn">⚠ arm targeting for auto-fire to act</span>}
            >
              <span class={`af-progress ${afStatus().locked_ms >= dwellMs() && afStatus().locked_ms > 0 ? 'ready' : ''}`}>
                {afStatus().locked_ms > 0
                  ? `locked ${(afStatus().locked_ms / 1000).toFixed(1)}s / ${(dwellMs() / 1000).toFixed(1)}s`
                  : 'waiting for lock…'}
              </span>
            </Show>
          </div>
        </Show>

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

        <div class="vision-row">
          <label>fallback aim</label>
          <input
            class="gain-input"
            type="number"
            step="0.5"
            placeholder="pan°"
            value={Number.isFinite(fbPanVal()) ? fbPanVal() : ''}
            onChange={(e) => setFbPan(e.currentTarget.value === '' ? NaN : parseFloat(e.currentTarget.value))}
          />
          <input
            class="gain-input"
            type="number"
            step="0.5"
            placeholder="tilt°"
            value={Number.isFinite(fbTiltVal()) ? fbTiltVal() : ''}
            onChange={(e) => setFbTilt(e.currentTarget.value === '' ? NaN : parseFloat(e.currentTarget.value))}
          />
          <button
            class="mini"
            disabled={!maintenanceActive() || !fbLive()}
            title={maintenanceActive() ? 'point the turret + gaze there to sight it in' : 'requires maintenance mode'}
            onClick={aimAtFallback}
          >
            aim there
          </button>
          <span class="muted small">
            {fbLive()
              ? 'vision down → gun parks + fires here (aim above the mic); Save to persist'
              : 'unset — a vision failure holds the shot instead'}
          </span>
        </div>

        <div class="vision-row">
          <label>tuning</label>
          <button disabled={!online() || !liveTuning()} onClick={() => void saveTuning()}>
            Save tuning
          </button>
          <button disabled={!online() || !savedTuning()} onClick={() => void reapplySaved()}>
            Re-apply saved
          </button>
          <Show
            when={tuningDirty()}
            fallback={<span class="muted small">{savedTuning() ? 'matches saved' : 'nothing saved yet'}</span>}
          >
            <span class="af-warn">unsaved changes — lost if vision restarts</span>
          </Show>
        </div>
        <div class="status-line">
          <Show when={tuneStatus()}><span class="status">{tuneStatus()}</span></Show>
          <Show when={tuneError()}><span class="err">{tuneError()}</span></Show>
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
          if the gun runs <em>away</em> from the target, flip that axis's gain sign. Edits apply
          immediately but live only in the vision process; <b>Save tuning</b> persists them
          (gains, tolerance, boresight, target, auto-fire dwell) to <code>vision.toml</code>,
          and the orchestrator re-seeds vision from there on every (re)connect. Trials acquire
          the <em>saved</em> target part.
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
