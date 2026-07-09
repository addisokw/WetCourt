import { createSignal, Match, onMount, Show, Switch } from 'solid-js';
import App from './App';
import JudgeMindPanel from './JudgeMindPanel';
import {
  enterMaintenance,
  exitMaintenance,
  fetchCalibrations,
  fetchDevices,
  maintenanceActive,
} from './maintenance';
import JudgeBodyPanel from './panels/JudgeBodyPanel';
import GavelPanel from './panels/GavelPanel';
import TurretPanel from './panels/TurretPanel';
import VisionPanel from './panels/VisionPanel';
import LawyerPanel from './panels/LawyerPanel';

type Tab = 'operator' | 'judge_mind' | 'vision' | 'lawyer' | 'judge_body' | 'gavel' | 'turret';
type Kind = 'operator' | 'config' | 'hardware';

// 'config' tabs hit the /operator/* endpoints and are safe live (ungated).
// 'hardware' tabs send direct device commands and require maintenance mode.
// The Vision feed is read-only monitoring, so it's ungated too.
const TABS: Array<{ id: Tab; label: string; kind: Kind }> = [
  { id: 'operator', label: 'Operator', kind: 'operator' },
  { id: 'judge_mind', label: 'Judge Mind', kind: 'config' },
  { id: 'vision', label: 'Vision', kind: 'config' },
  { id: 'lawyer', label: 'Lawyer', kind: 'config' },
  { id: 'judge_body', label: 'Judge Body', kind: 'hardware' },
  { id: 'gavel', label: 'Gavel', kind: 'hardware' },
  { id: 'turret', label: 'Turret', kind: 'hardware' },
];

export default function Shell() {
  const [tab, setTab] = createSignal<Tab>('operator');
  const [error, setError] = createSignal('');

  const kindOf = (id: Tab): Kind => TABS.find((t) => t.id === id)!.kind;

  onMount(() => {
    // Calibration is needed to render the aim sliders / fire presets; presence
    // is refreshed on entry too (it also streams in over /ws).
    void fetchCalibrations().catch(() => {});
    void fetchDevices();
  });

  async function doEnter() {
    setError('');
    try {
      await enterMaintenance();
      await fetchDevices();
    } catch (e) {
      setError(String(e));
    }
  }

  async function doExit() {
    setError('');
    try {
      await exitMaintenance();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div class="shell">
      <nav class="tab-bar">
        <div class="tab-group">
          {TABS.map((t, i) => (
            <>
              <Show when={i > 0 && t.kind === 'hardware' && TABS[i - 1].kind !== 'hardware'}>
                <span class="tab-divider" />
              </Show>
              <button class={`tab ${tab() === t.id ? 'active' : ''}`} onClick={() => setTab(t.id)}>
                {t.label}
                <Show when={t.kind === 'hardware'}>
                  <span class="tab-lock" title="requires maintenance mode">
                    {maintenanceActive() ? '🔧' : '🔒'}
                  </span>
                </Show>
              </button>
            </>
          ))}
        </div>
        <div class="maint-controls">
          <span class={`maint-indicator ${maintenanceActive() ? 'on' : 'off'}`}>
            <span class="dot" /> {maintenanceActive() ? 'MAINTENANCE' : 'live'}
          </span>
          <Show
            when={maintenanceActive()}
            fallback={<button onClick={doEnter}>Enter maintenance</button>}
          >
            <button class="estop" onClick={doExit}>Exit maintenance</button>
          </Show>
        </div>
      </nav>

      <Show when={error()}>
        <div class="shell-error">{error()}</div>
      </Show>

      <div class="shell-content">
        {/* Operator console stays mounted so its /ws connection + global keys
            persist across tab switches; hidden rather than unmounted. */}
        <div class="operator-host" style={{ display: tab() === 'operator' ? 'block' : 'none' }}>
          <App />
        </div>

        {/* Config tab — no maintenance gate (server-side, safe live). */}
        <Show when={tab() === 'judge_mind'}>
          <div class="maint-tab">
            <JudgeMindPanel />
          </div>
        </Show>

        {/* Vision feed — read-only monitoring, ungated. */}
        <Show when={tab() === 'vision'}>
          <div class="maint-tab">
            <VisionPanel />
          </div>
        </Show>

        {/* Lawyer phone — status + ring-out via /lawyer/* proxy, safe live. */}
        <Show when={tab() === 'lawyer'}>
          <div class="maint-tab">
            <LawyerPanel />
          </div>
        </Show>

        {/* Hardware tabs — gated behind maintenance mode. */}
        <Show when={kindOf(tab()) === 'hardware'}>
          <div class="maint-tab">
            <Show
              when={maintenanceActive()}
              fallback={
                <div class="maint-gate">
                  <p>Direct hardware control is disabled during a live show.</p>
                  <p class="muted">
                    Enter maintenance mode (only available from idle) to test this subsystem.
                  </p>
                  <button onClick={doEnter}>Enter maintenance</button>
                </div>
              }
            >
              <Switch>
                <Match when={tab() === 'judge_body'}><JudgeBodyPanel /></Match>
                <Match when={tab() === 'gavel'}><GavelPanel /></Match>
                <Match when={tab() === 'turret'}><TurretPanel /></Match>
              </Switch>
            </Show>
          </div>
        </Show>
      </div>
    </div>
  );
}
