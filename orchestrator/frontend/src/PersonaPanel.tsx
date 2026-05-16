import { createSignal, createMemo, For, Show, onMount } from 'solid-js';
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

const EMPTY: Persona = {
  id: '',
  display_name: '',
  system_prompt: '',
  guilty_bias: 0.5,
  tts_voice: 'af_heart',
  tts_speed: null,
};

export default function PersonaPanel() {
  const [open, setOpen] = createSignal(false);
  const [form, setForm] = createSignal<Persona>({ ...EMPTY });
  const [mode, setMode] = createSignal<'existing' | 'new'>('existing');
  // originalId is set when editing an existing persona (the id the form was loaded from).
  const [originalId, setOriginalId] = createSignal<string>('');
  const [status, setStatus] = createSignal<string>('');
  const [error, setError] = createSignal<string>('');
  const [testCharge, setTestCharge] = createSignal('Loitering with intent');
  const [testPlea, setTestPlea] = createSignal('I was just walking my dog.');
  const [testResult, setTestResult] = createSignal<TestResult | null>(null);

  const errors = createMemo(() => validatePersona(form()));
  const hasErrors = () => Object.keys(errors()).length > 0;

  function patch<K extends keyof Persona>(k: K, v: Persona[K]) {
    setForm({ ...form(), [k]: v });
  }

  async function loadActive() {
    setError(''); setStatus('loading…');
    try {
      await fetchPersonas();
      const p = await fetchActivePersona();
      setForm(p); setOriginalId(p.id); setMode('existing');
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
      setStatus(`selected ${p.id}`);
    } catch (e) {
      setError(String(e)); setStatus('');
    }
  }

  onMount(() => { void loadActive(); });

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
            <input type="text" value={form().tts_voice} onInput={(e) => patch('tts_voice', e.currentTarget.value)} />
            <Show when={errors().tts_voice}><span class="err">{errors().tts_voice}</span></Show>
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
                    <span class="intensity">intensity: {r().intensity.toFixed(2)}</span>
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
