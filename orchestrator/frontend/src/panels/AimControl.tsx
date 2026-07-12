import { createMemo, createSignal, onCleanup, onMount, Show } from 'solid-js';
import { calibrations, Role, sendCommand } from '../maintenance';
import { startGamepad } from '../gamepad';
import { degToRaw, stickToDeg } from './common';

/** Interpolation stream cadence — matches the gamepad aim-stream feel. */
const SMOOTH_TICK_MS = 33;
const RATE_MIN = 5;
const RATE_MAX = 180;
const RATE_DEFAULT = 45;

/** Step `cur` toward `target` by at most `step` (snaps on arrival). */
function approach(cur: number, target: number, step: number): number {
  const d = target - cur;
  if (Math.abs(d) <= step) return target;
  return cur + (d > 0 ? step : -step);
}

/**
 * Pan/tilt aim control: touch sliders + left-stick gamepad mapping. Sends
 * `AIM` (logical degrees → raw applied host-side) as a fire-and-forget stream.
 * Owns its own gamepad loop (onStick only) so the parent panel can map buttons
 * independently.
 *
 * Smooth mode: sliders/stick set a *target*; a 30 Hz loop glides the sent
 * position toward it at an adjustable °/s rate — gentler on the mechanics
 * than servo-speed jumps when testing large moves. Off = direct sends,
 * exactly as before.
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

  function send(p: number, t: number) {
    // Tenths are plenty; keeps float noise out of the JSON stream.
    p = Math.round(p * 10) / 10;
    t = Math.round(t * 10) / 10;
    setSentPan(p);
    setSentTilt(t);
    void sendCommand(props.role, { cmd: 'aim', pan: p, tilt: t }, true);
  }
  function setAim(p: number, t: number) {
    setPan(Math.round(p));
    setTilt(Math.round(t));
    if (!smooth()) send(Math.round(p), Math.round(t));
    // smooth: the glide loop streams toward the new target
  }
  function toggleSmooth(on: boolean) {
    setSmooth(on);
    // Turning it off mid-glide: finish the move immediately.
    if (!on && (sentPan() !== pan() || sentTilt() !== tilt())) send(pan(), tilt());
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
      const dt = (now - last) / 1000;
      last = now;
      if (!smooth()) return;
      if (sentPan() === pan() && sentTilt() === tilt()) return; // settled: stream nothing
      const step = rate() * dt;
      send(approach(sentPan(), pan(), step), approach(sentTilt(), tilt(), step));
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
