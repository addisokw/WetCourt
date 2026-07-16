import { createSignal, createMemo, For, Show, onMount } from 'solid-js';

interface Voice {
  id: string;
  label: string;
  group: string;
}
import {
  Persona,
  TestResult,
  activeId,
  applyPersona,
  createPersona,
  fetchActivePersona,
  fetchPersonas,
  personas,
  savePersona,
  selectPersona,
  testPersona,
  validatePersona,
} from './persona';
import { applyRobotParamsToGraph, ROBOT_DEFAULTS, ROBOT_FIELDS, RobotParams } from './robotParams';

const EMPTY: Persona = {
  id: '',
  display_name: '',
  system_prompt: '',
  guilty_bias: 0.5,
  tts_voice: 'af_heart',
  tts_speed: null,
  face_persona: 'honorable',
  robot: { ...ROBOT_DEFAULTS },
};

// LED-matrix eye themes the judge-face firmware ships (personas.py ORDER).
const FACE_PERSONAS = ['honorable', 'magistrate', 'cosmic', 'nullpointer', 'petunia'];

export default function PersonaPanel() {
  // Starts open: the Judge Mind switcher shows one editor at a time, so
  // reaching this panel already took an explicit click.
  const [open, setOpen] = createSignal(true);
  const [form, setForm] = createSignal<Persona>({ ...EMPTY });
  const [mode, setMode] = createSignal<'existing' | 'new'>('existing');
  // originalId is set when editing an existing persona (the id the form was loaded from).
  const [originalId, setOriginalId] = createSignal<string>('');
  const [status, setStatus] = createSignal<string>('');
  const [error, setError] = createSignal<string>('');
  const [testCharge, setTestCharge] = createSignal('Loitering with intent');
  const [testPlea, setTestPlea] = createSignal('I was just walking my dog.');
  const [testResult, setTestResult] = createSignal<TestResult | null>(null);
  const [voices, setVoices] = createSignal<Voice[]>([]);

  // Group voices by their `group` field for the optgroup labels in the select.
  const voiceGroups = createMemo(() => {
    const out = new Map<string, Voice[]>();
    for (const v of voices()) {
      if (!out.has(v.group)) out.set(v.group, []);
      out.get(v.group)!.push(v);
    }
    return out;
  });

  async function loadVoices() {
    try {
      const r = await fetch('/operator/voices');
      if (!r.ok) return;
      const body = await r.json();
      if (Array.isArray(body.voices)) setVoices(body.voices);
    } catch {
      // Non-fatal — the input falls back to free-text via the manual entry row.
    }
  }

  const errors = createMemo(() => validatePersona(form()));
  const hasErrors = () => Object.keys(errors()).length > 0;

  function patch<K extends keyof Persona>(k: K, v: Persona[K]) {
    setForm({ ...form(), [k]: v });
  }

  // Robot params are per-persona; editing them gives instant local audio
  // feedback (the form persona is always the active one, since the dropdown
  // selects on the backend) and persists with Apply/Save like any other field.
  function patchRobot<K extends keyof RobotParams>(k: K, v: number) {
    setForm({ ...form(), robot: { ...form().robot, [k]: v } });
    ROBOT_FIELDS.find((f) => f.key === k)?.apply(v);
  }

  async function loadActive() {
    setError(''); setStatus('loading…');
    try {
      await fetchPersonas();
      const p = await fetchActivePersona();
      setForm(p); setOriginalId(p.id); setMode('existing');
      applyRobotParamsToGraph(p.robot); // match local audio to the active persona
      setStatus(`loaded ${p.id}`);
    } catch (e) {
      setError(String(e)); setStatus('');
    }
  }

  async function loadById(id: string) {
    if (!id) return;
    setError(''); setStatus(`loading ${id}…`);
    try {
      // Select on backend, then fetch active to get full record.
      await selectPersona(id);
      const p = await fetchActivePersona();
      setForm(p); setOriginalId(p.id); setMode('existing');
      applyRobotParamsToGraph(p.robot); // match local audio to the newly active persona
      setStatus(`selected ${p.id}`);
    } catch (e) {
      setError(String(e)); setStatus('');
    }
  }

  onMount(() => {
    void loadActive();
    void loadVoices();
  });

  async function doSelect() {
    setError(''); setStatus('selecting…');
    try {
      await selectPersona(form().id);
      setStatus(`active = ${form().id}`);
    } catch (e) { setError(String(e)); setStatus(''); }
  }

  async function doApply() {
    setError(''); setStatus('applying…');
    if (hasErrors()) { setError('fix validation errors first'); setStatus(''); return; }
    try {
      const p = await applyPersona(form());
      setForm(p);
      setStatus(`applied ${p.id} (in-memory)`);
    } catch (e) { setError(String(e)); setStatus(''); }
  }

  async function doSave() {
    setError(''); setStatus('saving…');
    try {
      // Save persists current in-memory state to disk. We apply first if form has changed,
      // to make Save match what the operator sees in the form.
      if (!hasErrors()) await applyPersona(form());
      await savePersona(form().id);
      setStatus(`saved ${form().id} to disk`);
      await fetchPersonas();
    } catch (e) { setError(String(e)); setStatus(''); }
  }

  async function doCreate() {
    setError(''); setStatus('creating…');
    if (hasErrors()) { setError('fix validation errors first'); setStatus(''); return; }
    try {
      const p = await createPersona(form());
      setForm(p); setOriginalId(p.id); setMode('existing');
      await fetchPersonas();
      setStatus(`created ${p.id}`);
    } catch (e) { setError(String(e)); setStatus(''); }
  }

  function doDuplicate() {
    setError('');
    const cur = form();
    const newId = `${cur.id}_copy`.slice(0, 32);
    setForm({ ...cur, id: newId });
    setOriginalId('');
    setMode('new');
    setStatus(`duplicated → ${newId} (unsaved; click Create)`);
  }

  function doNew() {
    setError('');
    setForm({ ...EMPTY, id: '' });
    setOriginalId('');
    setMode('new');
    setStatus('new persona — fill out form and Create');
    setTestResult(null);
  }

  async function doTest() {
    setError(''); setStatus('testing…'); setTestResult(null);
    if (hasErrors()) { setError('fix validation errors first'); setStatus(''); return; }
    try {
      // Apply first so test uses current form state, then call test.
      if (mode() === 'existing') await applyPersona(form());
      const r = await testPersona(form(), testCharge(), testPlea());
      setTestResult(r);
      setStatus('test complete (no hardware / no TTS)');
    } catch (e) { setError(String(e)); setStatus(''); }
  }

  return (
    <section class="persona-panel">
      <button class="panel-toggle" onClick={() => setOpen(!open())}>
        {open() ? '▼' : '▶'} Persona Panel {activeId() ? `(active: ${activeId()})` : ''}
      </button>
      <Show when={open()}>
        <div class="panel-body">
          <div class="row-line">
            <label>persona</label>
            <select
              value={mode() === 'new' ? '' : form().id}
              onChange={(e) => loadById(e.currentTarget.value)}
              disabled={mode() === 'new'}
            >
              <Show when={mode() === 'new'}>
                <option value="">— new (unsaved) —</option>
              </Show>
              <For each={personas()}>
                {(p) => (
                  <option value={p.id}>
                    {activeId() === p.id ? '● ' : '  '}{p.display_name} ({p.id}){activeId() === p.id ? ' (active)' : ''}
                  </option>
                )}
              </For>
            </select>
            <div class="btn-row">
              <button onClick={doSelect} disabled={mode() === 'new' || !!errors().id}>Select</button>
              <button onClick={doApply} disabled={mode() === 'new' || hasErrors()}>Apply</button>
              <button onClick={doSave} disabled={mode() === 'new' || hasErrors()}>Save</button>
              <button onClick={doDuplicate} disabled={mode() === 'new'}>Duplicate</button>
              <button onClick={doNew}>New</button>
              <Show when={mode() === 'new'}>
                <button onClick={doCreate} disabled={hasErrors()}>Create</button>
              </Show>
            </div>
          </div>

          <div class="field">
            <label>id</label>
            <input
              type="text"
              value={form().id}
              onInput={(e) => patch('id', e.currentTarget.value)}
              disabled={mode() === 'existing' && !!originalId()}
              placeholder="lowercase_alnum_underscore"
            />
            <Show when={errors().id}><span class="err">{errors().id}</span></Show>
          </div>

          <div class="field">
            <label>display_name</label>
            <input type="text" value={form().display_name} onInput={(e) => patch('display_name', e.currentTarget.value)} />
            <Show when={errors().display_name}><span class="err">{errors().display_name}</span></Show>
          </div>

          <div class="field">
            <label>system_prompt <span class="muted">({form().system_prompt.length}/8000)</span></label>
            <textarea
              class="system-prompt"
              rows={14}
              value={form().system_prompt}
              onInput={(e) => patch('system_prompt', e.currentTarget.value)}
            />
            <Show when={errors().system_prompt}><span class="err">{errors().system_prompt}</span></Show>
          </div>

          <div class="field inline">
            <label>guilty_bias</label>
            <input
              type="range" min="0" max="1" step="0.01"
              value={form().guilty_bias}
              onInput={(e) => patch('guilty_bias', parseFloat(e.currentTarget.value))}
            />
            <span class="numeric">{form().guilty_bias.toFixed(2)}</span>
            <Show when={errors().guilty_bias}><span class="err">{errors().guilty_bias}</span></Show>
          </div>

          <div class="field">
            <label>tts_voice</label>
            <Show
              when={voices().length > 0}
              fallback={
                <input
                  type="text"
                  value={form().tts_voice}
                  onInput={(e) => patch('tts_voice', e.currentTarget.value)}
                />
              }
            >
              <select
                value={form().tts_voice}
                onChange={(e) => patch('tts_voice', e.currentTarget.value)}
              >
                <Show when={!voices().some((v) => v.id === form().tts_voice)}>
                  <option value={form().tts_voice}>{form().tts_voice} (custom)</option>
                </Show>
                <For each={Array.from(voiceGroups().entries())}>
                  {([group, vs]) => (
                    <optgroup label={group}>
                      <For each={vs}>
                        {(v) => <option value={v.id}>{v.label} ({v.id})</option>}
                      </For>
                    </optgroup>
                  )}
                </For>
              </select>
            </Show>
            <Show when={errors().tts_voice}><span class="err">{errors().tts_voice}</span></Show>
          </div>

          <div class="field">
            <label>LED eye theme <span class="muted">(judge-face matrix persona)</span></label>
            <select
              value={form().face_persona}
              onChange={(e) => patch('face_persona', e.currentTarget.value)}
            >
              <For each={FACE_PERSONAS}>{(t) => <option value={t}>{t}</option>}</For>
            </select>
          </div>

          <div class="field inline">
            <label>tts_speed</label>
            <input
              type="range" min="0.5" max="2.0" step="0.05"
              value={form().tts_speed ?? 1.0}
              disabled={form().tts_speed === null}
              onInput={(e) => patch('tts_speed', parseFloat(e.currentTarget.value))}
            />
            <span class="numeric">{form().tts_speed === null ? 'default' : form().tts_speed!.toFixed(2)}</span>
            <label class="checkbox">
              <input
                type="checkbox"
                checked={form().tts_speed === null}
                onChange={(e) => patch('tts_speed', e.currentTarget.checked ? null : 1.0)}
              /> default
            </label>
            <Show when={errors().tts_speed}><span class="err">{errors().tts_speed}</span></Show>
          </div>

          <div class="field">
            <label>robot voice <span class="muted">(this persona's TTS effect; live on this console's audio)</span></label>
            <div class="robot-controls">
              <For each={ROBOT_FIELDS}>
                {(f) => (
                  <label class="robot-row">
                    <span class="robot-label">{f.label}</span>
                    <input
                      type="range"
                      min={f.min}
                      max={f.max}
                      step={f.step}
                      value={form().robot[f.key]}
                      onInput={(e) => patchRobot(f.key, parseFloat(e.currentTarget.value))}
                    />
                    <span class="robot-num">{f.fmt(form().robot[f.key])}</span>
                  </label>
                )}
              </For>
            </div>
          </div>

          <div class="test-panel">
            <h3>Test deliberation</h3>
            <div class="muted small">
              Runs the model with the current form state (auto-Apply, in-memory).
              No hardware side effects, no TTS audio playback.
            </div>
            <div class="field">
              <label>charge</label>
              <input type="text" value={testCharge()} onInput={(e) => setTestCharge(e.currentTarget.value)} />
            </div>
            <div class="field">
              <label>plea</label>
              <textarea rows={3} value={testPlea()} onInput={(e) => setTestPlea(e.currentTarget.value)} />
            </div>
            <button onClick={doTest} disabled={mode() === 'new' || hasErrors()}>Test</button>
            <Show when={testResult()}>
              {(r) => (
                <div class="test-result">
                  <div class="test-meta">
                    <span class={`verdict ${r().guilty ? 'guilty' : 'not-guilty'}`}>
                      {r().guilty ? 'GUILTY' : 'NOT GUILTY'}
                    </span>
                    <Show when={r().key_factor}>
                      <span class="test-key-factor">key factor: {r().key_factor}</span>
                    </Show>
                  </div>
                  <div class="test-deliberation">{r().deliberation}</div>
                </div>
              )}
            </Show>
          </div>

          <div class="status-line">
            <Show when={status()}><span class="status">{status()}</span></Show>
            <Show when={error()}><span class="err">{error()}</span></Show>
          </div>
        </div>
      </Show>
    </section>
  );
}
