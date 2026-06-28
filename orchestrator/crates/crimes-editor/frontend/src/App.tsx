import { createMemo, createSignal, For, Show, onMount, type JSX } from 'solid-js';
import {
  Crime,
  addCrime,
  categories,
  crimes,
  deleteCrime,
  fetchCrimes,
  postCrime,
  putCrime,
  updateCrime,
  validateCategory,
  validateCharge,
  validateSubject,
} from './crimes';
import { crimesToCsv, parseCrimesCsv } from './csv';

type GroupBy = 'none' | 'category' | 'subject';
const NO_SUBJECT = '(no subject)';

// The crimes panel from the operator console, expanded into a focused,
// full-page editor. Same SolidJS stack and CSS classes as the console, minus
// the live-trial controls (charge queue, draw filter) that need a running booth.
export default function App() {
  const [status, setStatus] = createSignal('');
  const [error, setError] = createSignal('');

  // add form
  const [newCategory, setNewCategory] = createSignal('');
  const [newCharge, setNewCharge] = createSignal('');
  const [newSubject, setNewSubject] = createSignal('');
  // list view filter (client-side browsing)
  const [viewCategory, setViewCategory] = createSignal('');
  // free-text search over the visible list
  const [search, setSearch] = createSignal('');
  // grouping
  const [groupBy, setGroupBy] = createSignal<GroupBy>('none');
  // inline edit
  const [editing, setEditing] = createSignal<Crime | null>(null);

  let fileInput!: HTMLInputElement;

  onMount(() => {
    void refresh();
  });

  async function refresh() {
    setError('');
    try {
      await fetchCrimes();
    } catch (e) {
      setError(String(e));
    }
  }

  async function run(label: string, fn: () => Promise<void>) {
    setError('');
    setStatus(`${label}…`);
    try {
      await fn();
      setStatus(label + ' done');
    } catch (e) {
      setError(String(e));
      setStatus('');
    }
  }

  const enabledCount = createMemo(() => crimes().filter((c) => c.enabled).length);
  const visible = createMemo(() => {
    const q = search().trim().toLowerCase();
    return crimes().filter(
      (c) =>
        (!viewCategory() || c.category === viewCategory()) &&
        (!q ||
          c.charge.toLowerCase().includes(q) ||
          c.category.toLowerCase().includes(q) ||
          (c.subject ?? '').toLowerCase().includes(q)),
    );
  });

  // The visible set, optionally bucketed. `key === null` is the single
  // ungrouped bucket (no header rendered).
  const groups = createMemo<{ key: string | null; items: Crime[] }[]>(() => {
    const items = visible();
    const mode = groupBy();
    if (mode === 'none') return [{ key: null, items }];
    const map = new Map<string, Crime[]>();
    for (const c of items) {
      const k = mode === 'subject' ? c.subject?.trim() || NO_SUBJECT : c.category;
      const bucket = map.get(k);
      if (bucket) bucket.push(c);
      else map.set(k, [c]);
    }
    return [...map.keys()]
      .sort((a, b) => (a === NO_SUBJECT ? 1 : b === NO_SUBJECT ? -1 : a.localeCompare(b)))
      .map((key) => ({ key, items: map.get(key)! }));
  });

  const addErr = () =>
    newCharge() || newCategory() || newSubject()
      ? validateCharge(newCharge()) ??
        validateCategory(newCategory()) ??
        validateSubject(newSubject())
      : null;

  function exportCsv() {
    const rows = visible();
    const blob = new Blob([crimesToCsv(rows)], { type: 'text/csv;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `wet-court-crimes-${new Date().toISOString().slice(0, 10)}.csv`;
    a.click();
    URL.revokeObjectURL(url);
    setStatus(`exported ${rows.length} crime${rows.length === 1 ? '' : 's'}`);
  }

  async function onImportFile(e: Event & { currentTarget: HTMLInputElement }) {
    const file = e.currentTarget.files?.[0];
    e.currentTarget.value = ''; // allow re-importing the same file
    if (!file) return;
    setError('');
    setStatus('reading…');

    const { rows, errors: parseErrors } = parseCrimesCsv(await file.text());
    if (rows.length === 0) {
      setStatus('');
      setError(`CSV import: ${parseErrors.join('; ') || 'nothing to import'}`);
      return;
    }

    // Split into add vs update, validating each row the way the server would.
    const byId = new Map(crimes().map((c) => [c.id, c]));
    const toAdd: typeof rows = [];
    const toUpdate: typeof rows = [];
    const skipped: string[] = [...parseErrors];
    rows.forEach((r, i) => {
      const v =
        validateCharge(r.charge) ??
        validateCategory(r.category) ??
        validateSubject(r.subject ?? '');
      if (v) {
        skipped.push(`row ${i + 2}: ${v}`);
        return;
      }
      if (r.id != null && byId.has(r.id)) toUpdate.push(r);
      else toAdd.push(r);
    });

    if (toAdd.length === 0 && toUpdate.length === 0) {
      setStatus('');
      setError(`CSV import: no valid rows.\n${skipped.slice(0, 10).join('\n')}`);
      return;
    }
    const summary =
      `Import ${toAdd.length} new + ${toUpdate.length} updated crime(s)` +
      (skipped.length ? `, skipping ${skipped.length} invalid` : '') +
      '?';
    if (!confirm(summary + (skipped.length ? `\n\n${skipped.slice(0, 10).join('\n')}` : ''))) {
      setStatus('');
      return;
    }

    setStatus('importing…');
    try {
      for (const r of toAdd) await postCrime(r.category, r.charge, r.subject);
      for (const r of toUpdate) {
        const ex = byId.get(r.id!)!;
        await putCrime({
          id: r.id!,
          category: r.category,
          charge: r.charge,
          subject: r.subject,
          enabled: r.enabled ?? ex.enabled,
        });
      }
      await fetchCrimes();
      setStatus(
        `import done: +${toAdd.length} added, ${toUpdate.length} updated` +
          (skipped.length ? `, ${skipped.length} skipped` : ''),
      );
    } catch (err) {
      setError(`import failed partway: ${String(err)}`);
      setStatus('');
      await refresh();
    }
  }

  // One crime's row — shared by the flat and grouped renderings.
  function crimeRow(c: Crime): JSX.Element {
    return (
      <li class={c.enabled ? '' : 'disabled'}>
        <Show
          when={editing()?.id === c.id}
          fallback={
            <>
              <label class="checkbox" title="enabled — eligible for the booth draw">
                <input
                  type="checkbox"
                  checked={c.enabled}
                  onChange={(e) =>
                    run('toggle', () => updateCrime({ ...c, enabled: e.currentTarget.checked }))
                  }
                />
              </label>
              <span class="crime-cat">{c.category}</span>
              <Show when={c.subject}>
                <span class="crime-subject">{c.subject}</span>
              </Show>
              <span class="crime-text">{c.charge}</span>
              <button class="mini" onClick={() => setEditing({ ...c })}>
                edit
              </button>
              <button
                class="mini danger"
                onClick={() => {
                  if (confirm(`Delete crime #${c.id}?\n\n${c.charge}`)) {
                    void run('delete', () => deleteCrime(c.id));
                  }
                }}
              >
                delete
              </button>
            </>
          }
        >
          {(_) => {
            const e = () => editing()!;
            return (
              <div class="crime-edit">
                <input
                  type="text"
                  class="category-input"
                  list="crime-categories"
                  value={e().category}
                  onInput={(ev) => setEditing({ ...e(), category: ev.currentTarget.value })}
                />
                <input
                  type="text"
                  placeholder="subject (optional)"
                  value={e().subject ?? ''}
                  onInput={(ev) => setEditing({ ...e(), subject: ev.currentTarget.value || null })}
                />
                <textarea
                  rows={2}
                  value={e().charge}
                  onInput={(ev) => setEditing({ ...e(), charge: ev.currentTarget.value })}
                />
                <div class="btn-row">
                  <button
                    disabled={
                      !!validateCharge(e().charge) ||
                      !!validateCategory(e().category) ||
                      !!validateSubject(e().subject ?? '')
                    }
                    onClick={() =>
                      run('save', async () => {
                        await updateCrime(e());
                        setEditing(null);
                      })
                    }
                  >
                    Save
                  </button>
                  <button onClick={() => setEditing(null)}>Cancel</button>
                </div>
                <Show
                  when={
                    validateCharge(e().charge) ??
                    validateCategory(e().category) ??
                    validateSubject(e().subject ?? '')
                  }
                >
                  {(msg) => <span class="err">{msg()}</span>}
                </Show>
              </div>
            );
          }}
        </Show>
      </li>
    );
  }

  return (
    <div class="app">
      <header>
        <h1 class="banner">Wet Court — Crimes</h1>
        <span class="muted">
          {enabledCount()}/{crimes().length} enabled · {categories().length} categories
        </span>
      </header>

      <section class="persona-panel crimes-panel">
        <div class="panel-body">
          {/* Add a new crime */}
          <div class="field">
            <label>add a crime</label>
            <div class="row-line">
              <input
                type="text"
                class="category-input"
                list="crime-categories"
                placeholder="category"
                value={newCategory()}
                onInput={(e) => setNewCategory(e.currentTarget.value)}
              />
              <datalist id="crime-categories">
                <For each={categories()}>{(c) => <option value={c} />}</For>
              </datalist>
              <input
                type="text"
                class="category-input"
                placeholder="subject (optional)"
                value={newSubject()}
                onInput={(e) => setNewSubject(e.currentTarget.value)}
              />
              <input
                type="text"
                placeholder="The defendant stands accused of…"
                value={newCharge()}
                onInput={(e) => setNewCharge(e.currentTarget.value)}
              />
              <button
                disabled={!newCharge() || !newCategory() || !!addErr()}
                onClick={() =>
                  run('add', async () => {
                    await addCrime(newCategory().trim(), newCharge().trim(), newSubject());
                    setNewCharge('');
                    setNewSubject('');
                  })
                }
              >
                Add
              </button>
            </div>
            <Show when={addErr()}>{(msg) => <span class="err">{msg()}</span>}</Show>
          </div>

          {/* Browse + curate the list */}
          <div class="field inline">
            <label>browse</label>
            <select value={viewCategory()} onChange={(e) => setViewCategory(e.currentTarget.value)}>
              <option value="">all ({crimes().length})</option>
              <For each={categories()}>
                {(c) => (
                  <option value={c}>
                    {c} ({crimes().filter((x) => x.category === c).length})
                  </option>
                )}
              </For>
            </select>
            <input
              type="search"
              class="crime-search"
              placeholder="search charges…"
              value={search()}
              onInput={(e) => setSearch(e.currentTarget.value)}
            />
            <Show when={search().trim() || viewCategory()}>
              <span class="muted small">
                {visible().length} match{visible().length === 1 ? '' : 'es'}
              </span>
            </Show>
          </div>

          {/* Group + CSV tools */}
          <div class="field inline">
            <label>group by</label>
            <select value={groupBy()} onChange={(e) => setGroupBy(e.currentTarget.value as GroupBy)}>
              <option value="none">none</option>
              <option value="category">category</option>
              <option value="subject">subject / creator</option>
            </select>
            <div class="btn-row">
              <button onClick={exportCsv}>Export CSV ({visible().length})</button>
              <button onClick={() => fileInput.click()}>Import CSV</button>
            </div>
            <input
              ref={fileInput}
              type="file"
              accept=".csv,text/csv"
              style={{ display: 'none' }}
              onChange={onImportFile}
            />
          </div>

          <ul class="crime-list">
            <For each={groups()}>
              {(g) => (
                <>
                  <Show when={g.key !== null}>
                    <li class="crime-group">
                      <span class="group-name">{g.key}</span>
                      <span class="muted small">
                        {g.items.filter((c) => c.enabled).length}/{g.items.length}
                      </span>
                    </li>
                  </Show>
                  <For each={g.items}>{(c) => crimeRow(c)}</For>
                </>
              )}
            </For>
          </ul>

          <div class="status-line">
            <Show when={status()}>
              <span class="status">{status()}</span>
            </Show>
            <Show when={error()}>
              <span class="err">{error()}</span>
            </Show>
          </div>
        </div>
      </section>
    </div>
  );
}
