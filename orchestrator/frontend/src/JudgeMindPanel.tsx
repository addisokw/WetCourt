import { For, onMount } from 'solid-js';
import PersonaPanel from './PersonaPanel';
import CrimesPanel from './CrimesPanel';
import { crossExamEnabled, fetchCrossExam, setCrossExam } from './ws';
import { applyParam, getParam, resetRobotParams, ROBOT_PARAMS } from './robotSettings';

/** Robot-TTS voice colour. Local to this browser's audio output. */
function RobotControls() {
  return (
    <div class="robot-controls">
      <For each={ROBOT_PARAMS}>
        {(p) => (
          <label class="robot-row">
            <span class="robot-label">{p.label}</span>
            <input
              type="range"
              min={p.min}
              max={p.max}
              step={p.step}
              value={getParam(p.key)}
              onInput={(e) => applyParam(p.key, parseFloat(e.currentTarget.value))}
            />
            <span class="robot-num">{p.fmt(getParam(p.key))}</span>
          </label>
        )}
      </For>
      <div class="btn-row">
        <button onClick={resetRobotParams}>Reset to default</button>
      </div>
    </div>
  );
}

export default function JudgeMindPanel() {
  // Keep the cross-exam toggle in sync with the server when this tab opens.
  onMount(() => void fetchCrossExam());

  return (
    <div class="config-tab">
      <p class="muted small">
        The judge's mind: who it is, what it tries people for, and how it speaks.
        Persona and case settings are server-side; the robot voice effect is local
        to this browser's audio.
      </p>

      <section class="judge-section">
        <h3>Behavior</h3>
        <label class="checkbox-row" title="Judge asks one follow-up question after the plea">
          <input
            type="checkbox"
            checked={crossExamEnabled()}
            onChange={(e) => void setCrossExam(e.currentTarget.checked)}
          />
          <span>Cross-examination — one follow-up question after the plea</span>
        </label>
      </section>

      <section class="judge-section">
        <h3>Robot voice <span class="muted small">(this browser's TTS audio)</span></h3>
        <RobotControls />
      </section>

      <PersonaPanel />
      <CrimesPanel />
    </div>
  );
}
