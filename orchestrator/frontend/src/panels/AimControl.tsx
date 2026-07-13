import { createMemo, createSignal, onCleanup, onMount, Show } from 'solid-js';
import { calibrations, Role, sendCommand } from '../maintenance';
import { startGamepad } from '../gamepad';
import { degToRaw, stickToDeg } from './common';

/** Interpolation stream cadence — matches the gamepad aim-stream feel. */
const SMOOTH_TICK_MS = 33;
const RATE_MIN = 5;
const RATE_MAX = 180;
const RATE_DEFAULT = 45;
/** Seconds to reach cruise speed — sets the ease-in/out acceleration. */
const ACCEL_SECS = 0.3;

/**
 * One tick of an acceleration-limited approach: ease in from rest, cruise at
 * `vmax`, and brake (√(2a·d) envelope) into the target — ease-in/out that
 * stays smooth when the target moves mid-glide (stick input retargets every
 * frame, where a time-based curve would stutter). Returns [next, velocity];
 * arrival snaps exactly onto the target with zero velocity.
 */
function ease(
  cur: number,
  target: number,
  v: number,
  vmax: number,
  dt: number,
): [number, number] {
  const amax = vmax / ACCEL_SECS;
  const d = target - cur;
  // The fastest speed that can still brake to a stop at the target.
  const want = Math.sign(d) * Math.min(vmax, Math.sqrt(2 * amax * Math.abs(d)));
  v += Math.max(-amax * dt, Math.min(amax * dt, want - v));
  const next = cur + v * dt;
  // Close and slow → settle exactly (kills sub-resolution creep at the end).
  if (Math.abs(target - next) <= 0.1 && Math.abs(v) <= 2 * amax * dt) {
    return [target, 0];
  }
  return [next, v];
}

/**
 * Pan/tilt aim control: touch sliders + left-stick gamepad mapping. Sends
 * `AIM` (logical degrees → raw applied host-side) as a fire-and-forget stream.
 * Owns its own gamepad loop (onStick only) so the parent panel can map buttons
 * independently.
 *
 * Smooth mode: sliders/stick set a *target*; a 30 Hz loop eases the sent
 * position toward it — accelerating in, cruising at the adjustable °/s rate,
 * braking out — gentler on the mechanics than servo-speed jumps when testing
 * large moves. Off = direct sends, exactly as before.
 */
export default function AimControl(props: { role: Role }) {
  const cal = createMemo(() => calibrations()[props.role]);
  const panCal = () => cal()?.pan ?? null;
  const tiltCal = () => cal()?.tilt ?? null;

  // Target (what the sliders show) vs sent (what the hardware was last told).
  // They only differ mid-glide in smooth mode.
  const [pan, setPan] = createSignal(0);
  const [tilt, setTilt] = createSignal(0);
  const [sentPan, setSentPan] = createSignal(0);
  const [sentTilt, setSentTilt] = createSignal(0);
  const [smooth, setSmooth] = createSignal(false);
  const [rate, setRate] = createSignal(RATE_DEFAULT); // °/s

  // Precise glide state (unrounded position + velocity per axis) — the wire
  // stream rounds to tenths, which would stall the gentle ease-in steps at
  // low rates if it were also the integration state.
  let curP = 0;
  let curT = 0;
  let vP = 0;
  let vT = 0;

  function send(p: number, t: number) {
    // Tenths are plenty; keeps float noise out of the JSON stream.
    p = Math.round(p * 10) / 10;
    t = Math.round(t * 10) / 10;
    setSentPan(p);
    setSentTilt(t);
    void sendCommand(props.role, { cmd: 'aim', pan: p, tilt: t }, true);
  }
  /** Direct (non-glide) jump: sync the glide state so a later glide starts here. */
  function jumpTo(p: number, t: number) {
    curP = p;
    curT = t;
    vP = 0;
    vT = 0;
    send(p, t);
  }
  function setAim(p: number, t: number) {
    setPan(Math.round(p));
    setTilt(Math.round(t));
    if (!smooth()) jumpTo(Math.round(p), Math.round(t));
    // smooth: the glide loop eases toward the new target
  }
  function toggleSmooth(on: boolean) {
    setSmooth(on);
    // Turning it off mid-glide: finish the move immediately.
    if (!on && (sentPan() !== pan() || sentTilt() !== tilt())) jumpTo(pan(), tilt());
  }

  onMount(() => {
    const stop = startGamepad({
      onStick: (x, y) => {
        // Invert Y so pushing up tilts up (negative axis = up on a gamepad).
        setAim(stickToDeg(x, panCal()), stickToDeg(-y, tiltCal()));
      },
    });
    let last = performance.now();
    const timer = window.setInterval(() => {
      const now = performance.now();
      const dt = Math.min((now - last) / 1000, 0.25); // clamp tab-hidden hitches
      last = now;
      if (!smooth()) return;
      if (curP === pan() && curT === tilt() && vP === 0 && vT === 0) return; // settled
      [curP, vP] = ease(curP, pan(), vP, rate(), dt);
      [curT, vT] = ease(curT, tilt(), vT, rate(), dt);
      send(curP, curT);
    }, SMOOTH_TICK_MS);
    onCleanup(() => {
      stop();
      clearInterval(timer);
    });
  });

  const gliding = () => smooth() && (sentPan() !== pan() || sentTilt() !== tilt());

  return (
    <div class="aim-control">
      <Show when={panCal() && tiltCal()} fallback={<div class="muted small">no pan/tilt axes</div>}>
        <label class="aim-row">
          <span>
            pan {pan()}° <span class="aim-raw">{degToRaw(sentPan(), panCal())} µs</span>
          </span>
          <input
            type="range"
            min={panCal()!.limit_min_deg}
            max={panCal()!.limit_max_deg}
            step={1}
            value={pan()}
            onInput={(e) => setAim(parseFloat(e.currentTarget.value), tilt())}
          />
        </label>
        <label class="aim-row">
          <span>
            tilt {tilt()}° <span class="aim-raw">{degToRaw(sentTilt(), tiltCal())} µs</span>
          </span>
          <input
            type="range"
            min={tiltCal()!.limit_min_deg}
            max={tiltCal()!.limit_max_deg}
            step={1}
            value={tilt()}
            onInput={(e) => setAim(pan(), parseFloat(e.currentTarget.value))}
          />
        </label>
        <div class="btn-row">
          <button onClick={() => setAim(0, 0)}>Center</button>
          <label class="checkbox" title="glide toward the target instead of jumping">
            <input
              type="checkbox"
              checked={smooth()}
              onChange={(e) => toggleSmooth(e.currentTarget.checked)}
            />{' '}
            smooth
          </label>
          <Show when={smooth()}>
            <input
              type="range"
              class="aim-rate"
              min={RATE_MIN}
              max={RATE_MAX}
              step={5}
              value={rate()}
              onInput={(e) => setRate(parseFloat(e.currentTarget.value))}
            />
            <span class={`muted small aim-rate-label ${gliding() ? 'gliding' : ''}`}>
              {rate()}°/s{gliding() ? ' · moving' : ''}
            </span>
          </Show>
        </div>
        <div class="muted small">Left stick aims; release to hold. Touch sliders also work.</div>
      </Show>
    </div>
  );
}
