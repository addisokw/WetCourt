import { createMemo, createSignal, onCleanup, onMount, Show } from 'solid-js';
import { calibrations, Role, sendCommand } from '../maintenance';
import { startGamepad } from '../gamepad';
import { stickToDeg } from './common';

/**
 * Pan/tilt aim control: touch sliders + left-stick gamepad mapping. Sends
 * `AIM` (logical degrees → raw applied host-side) as a fire-and-forget stream.
 * Owns its own gamepad loop (onStick only) so the parent panel can map buttons
 * independently.
 */
export default function AimControl(props: { role: Role }) {
  const cal = createMemo(() => calibrations()[props.role]);
  const panCal = () => cal()?.pan ?? null;
  const tiltCal = () => cal()?.tilt ?? null;

  const [pan, setPan] = createSignal(0);
  const [tilt, setTilt] = createSignal(0);

  function send(p: number, t: number) {
    void sendCommand(props.role, { cmd: 'aim', pan: p, tilt: t }, true);
  }
  function setAim(p: number, t: number) {
    setPan(Math.round(p));
    setTilt(Math.round(t));
    send(Math.round(p), Math.round(t));
  }

  onMount(() => {
    const stop = startGamepad({
      onStick: (x, y) => {
        // Invert Y so pushing up tilts up (negative axis = up on a gamepad).
        setAim(stickToDeg(x, panCal()), stickToDeg(-y, tiltCal()));
      },
    });
    onCleanup(stop);
  });

  return (
    <div class="aim-control">
      <Show when={panCal() && tiltCal()} fallback={<div class="muted small">no pan/tilt axes</div>}>
        <label class="aim-row">
          <span>pan {pan()}°</span>
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
          <span>tilt {tilt()}°</span>
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
        </div>
        <div class="muted small">Left stick aims; release to hold. Touch sliders also work.</div>
      </Show>
    </div>
  );
}
