import { onMount } from 'solid-js';
import PersonaPanel from './PersonaPanel';
import CrimesPanel from './CrimesPanel';
import { crossExamEnabled, fetchCrossExam, setCrossExam } from './ws';

export default function JudgeMindPanel() {
  // Keep the cross-exam toggle in sync with the server when this tab opens.
  onMount(() => void fetchCrossExam());

  return (
    <div class="config-tab">
      <p class="muted small">
        The judge's mind: who it is, what it tries people for, and how it speaks.
        Persona (including its robot voice effect) and case settings are stored on
        the host; the active persona's voice is applied to playback automatically.
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

      <PersonaPanel />
      <CrimesPanel />
    </div>
  );
}
