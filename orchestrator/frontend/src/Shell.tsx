import { createSignal, Match, onMount, Show, Switch } from 'solid-js';
import App from './App';
import {
  enterMaintenance,
  exitMaintenance,
  fetchCalibrations,
  fetchDevices,
  maintenanceActive,
} from './maintenance';
import AiJudgePanel from './panels/AiJudgePanel';
import GavelPanel from './panels/GavelPanel';
import TurretPanel from './panels/TurretPanel';

type Tab = 'operator' | 'ai_judge' | 'gavel' | 'turret';

const TABS: Array<{ id: Tab; label: string }> = [
  { id: 'operator', label: 'Operator' },
  { id: 'ai_judge', label: 'AI Judge' },
  { id: 'gavel', label: 'Gavel' },
  { id: 'turret', label: 'Turret' },
];

export default function Shell() {
  const [tab, setTab] = createSignal<Tab>('operator');
  const [error, setError] = createSignal('');

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
          {TABS.map((t) => (
            <button class={`tab ${tab() === t.id ? 'active' : ''}`} onClick={() => setTab(t.id)}>
              {t.label}
            </button>
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

        <Show when={tab() !== 'operator'}>
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
                <Match when={tab() === 'ai_judge'}><AiJudgePanel /></Match>
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
