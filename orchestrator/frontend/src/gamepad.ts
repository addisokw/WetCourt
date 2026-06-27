// Browser Gamepad API driver for the maintenance console. The Steam Deck runs
// the console in a Desktop-Mode browser, so its sticks/buttons appear here as a
// standard gamepad. A single requestAnimationFrame loop polls the pad; only the
// active panel starts it (onMount) and stops it (onCleanup), so at most one loop
// runs at a time.

export interface GamepadMapping {
  /** Left stick, normalised to [-1, 1] with deadzone applied. Throttled to
   *  ~20 Hz and only fired on meaningful change (plus a final settle to 0,0). */
  onStick?: (x: number, y: number) => void;
  /** Rising edge of a button press. Standard mapping indices: 0=A, 1=B, 2=X,
   *  3=Y, 4=LB, 5=RB, 6=LT, 7=RT, 9=Start. */
  onButtonDown?: (index: number) => void;
}

const DEADZONE = 0.15;
const STICK_THROTTLE_MS = 50; // ~20 Hz
const STICK_EPSILON = 0.02;

function applyDeadzone(v: number): number {
  if (Math.abs(v) < DEADZONE) return 0;
  // Rescale so the edge of the deadzone is 0 and full deflection stays 1.
  const sign = Math.sign(v);
  return sign * ((Math.abs(v) - DEADZONE) / (1 - DEADZONE));
}

/**
 * Start polling the first connected gamepad. Returns a stop function.
 * `performance.now()` is used for throttling (allowed in the browser).
 */
export function startGamepad(map: GamepadMapping): () => void {
  let raf = 0;
  let lastButtons: boolean[] = [];
  let lastStickAt = 0;
  let lastX = 0;
  let lastY = 0;
  let stickWasNonZero = false;

  const tick = () => {
    const pads = navigator.getGamepads?.() ?? [];
    const gp = Array.from(pads).find((p) => p && p.connected) || null;

    if (gp) {
      // Buttons — rising-edge detection.
      if (map.onButtonDown) {
        for (let i = 0; i < gp.buttons.length; i++) {
          const pressed = gp.buttons[i]?.pressed ?? false;
          if (pressed && !lastButtons[i]) map.onButtonDown(i);
          lastButtons[i] = pressed;
        }
      }

      // Left stick — deadzone + throttle + change threshold.
      if (map.onStick) {
        const x = applyDeadzone(gp.axes[0] ?? 0);
        const y = applyDeadzone(gp.axes[1] ?? 0);
        const now = performance.now();
        const moved = Math.abs(x - lastX) > STICK_EPSILON || Math.abs(y - lastY) > STICK_EPSILON;
        const nonZero = x !== 0 || y !== 0;
        const settled = stickWasNonZero && !nonZero; // returned to center — send once

        if ((moved && now - lastStickAt >= STICK_THROTTLE_MS) || settled) {
          map.onStick(x, y);
          lastStickAt = now;
          lastX = x;
          lastY = y;
          stickWasNonZero = nonZero;
        }
      }
    }

    raf = requestAnimationFrame(tick);
  };

  raf = requestAnimationFrame(tick);
  return () => cancelAnimationFrame(raf);
}
