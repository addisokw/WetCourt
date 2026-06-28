import { createMemo, createSignal, For, Show, onMount } from 'solid-js';
import {
  Crime,
  addCrime,
  categories,
  categoryFilter,
  chargeQueue,
  crimes,
  deleteCrime,
  fetchCrimes,
  queueCharge,
  setCrimeFilter,
  unqueueCharge,
  updateCrime,
  validateCategory,
  validateCharge,
} from './crimes';

export default function CrimesPanel() {
  const [open, setOpen] = createSignal(false);
  const [status, setStatus] = createSignal('');
  const [error, setError] = createSignal('');

  // queue-next input
  const [queueText, setQueueText] = createSignal('');
  // add form
  const [newCategory, setNewCategory] = createSignal('');
  const [newCharge, setNewCharge] = createSignal('');
  const [newSubject, setNewSubject] = createSignal('');
  // list view filter (client-side browsing only; separate from the draw filter)
  const [viewCategory, setViewCategory] = createSignal('');
  // free-text search over the visible list (client-side only)
  const [search, setSearch] = createSignal('');
  // inline edit
  const [editing, setEditing] = createSignal<Crime | null>(null);

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
          (c.subject ?? '').toLowerCase().includes(q))
    );
  });

  const queueErr = () => (queueText() ? validateCharge(queueText()) : null);
  const addErr = () =>
    newCharge() || newCategory()
      ? validateCharge(newCharge()) ?? validateCategory(newCategory())
      : null;

  return (
    <section class="persona-panel crimes-panel">
      <button class="panel-toggle" onClick={() => setOpen(!open())}>
        {open() ? '▼' : '▶'} Crimes Panel ({enabledCount()}/{crimes().length} enabled
        {categoryFilter() ? `, drawing only: ${categoryFilter()}` : ''})
      </button>
      <Show when={open()}>
        <div class="panel-body">
          {/* Manual charge queue — next trial uses these before any draw */}
          <div class="field">
            <label>queue a charge for the next trial</label>
            <div class="row-line">
              <input
                type="text"
                placeholder="The defendant stands accused of…"
                value={queueText()}
                onInput={(e) => setQueueText(e.currentTarget.value)}
              />
              <button
                disabled={!queueText() || !!queueErr()}
                onClick={() =>
                  run('queue', async () => {
                    await queueCharge(queueText());
                    setQueueText('');
                  })
                }
              >
                Queue
              </button>
            </div>
            <Show when={queueErr()}><span class="err">{queueErr()}</span></Show>
            <Show when={chargeQueue().length > 0}>
              <ol class="charge-queue">
                <For each={chargeQueue()}>
                  {(q, i) => (
                    <li>
                      <span>{q}</span>
                      <button class="mini" title="remove from queue"
                        onClick={() => run('unqueue', () => unqueueCharge(i()))}>✕</button>
                    </li>
                  )}
                </For>
              </ol>
            </Show>
          </div>

          {/* Draw filter — restricts random selection (creator mode etc.) */}
          <div class="field inline">
            <label>draw only from</label>
            <select
              value={categoryFilter() ?? ''}
              onChange={(e) =>
                run('filter', () => setCrimeFilter(e.currentTarget.value || null))
              }
            >
              <option value="">all categories</option>
              <For each={categories()}>{(c) => <option value={c}>{c}</option>}</For>
            </select>
            <span class="muted small">
              random draws come only from this category until cleared
            </span>
          </div>

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
            <Show when={addErr()}><span class="err">{addErr()}</span></Show>
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
            <Show when={search().trim()}>
              <span class="muted small">{visible().length} match{visible().length === 1 ? '' : 'es'}</span>
            </Show>
          </div>

          <ul class="crime-list">
            <For each={visible()}>
              {(c) => (
                <li class={c.enabled ? '' : 'disabled'}>
                  <Show
                    when={editing()?.id === c.id}
                    fallback={
                      <>
                        <label class="checkbox" title="enabled — eligible for random draw">
                          <input
                            type="checkbox"
                            checked={c.enabled}
                            onChange={(e) =>
                              run('toggle', () =>
                                updateCrime({ ...c, enabled: e.currentTarget.checked })
                              )
                            }
                          />
                        </label>
                        <span class="crime-cat">{c.category}</span>
                        <Show when={c.subject}>
                          <span class="crime-subject">{c.subject}</span>
                        </Show>
                        <span class="crime-text">{c.charge}</span>
                        <button
                          class="mini"
                          title="queue this charge for the next trial"
                          onClick={() => run('queue', () => queueCharge(c.charge))}
                        >
                          queue
                        </button>
                        <button class="mini" onClick={() => setEditing({ ...c })}>edit</button>
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
                            onInput={(ev) =>
                              setEditing({ ...e(), subject: ev.currentTarget.value || null })
                            }
                          />
                          <textarea
                            rows={2}
                            value={e().charge}
                            onInput={(ev) => setEditing({ ...e(), charge: ev.currentTarget.value })}
                          />
                          <div class="btn-row">
                            <button
                              disabled={!!validateCharge(e().charge) || !!validateCategory(e().category)}
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
                          <Show when={validateCharge(e().charge) ?? validateCategory(e().category)}>
                            {(msg) => <span class="err">{msg()}</span>}
                          </Show>
                        </div>
                      );
                    }}
                  </Show>
                </li>
              )}
            </For>
          </ul>

          <div class="status-line">
            <Show when={status()}><span class="status">{status()}</span></Show>
            <Show when={error()}><span class="err">{error()}</span></Show>
          </div>
        </div>
      </Show>
    </section>
  );
}
