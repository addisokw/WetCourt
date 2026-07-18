import { createMemo, createSignal, Index, onMount, Show } from 'solid-js';
import { couponFrequency, fetchCoupons, setCoupons } from '../ws';
import {
  BannerBlock,
  BarcodeBlock,
  DOTS_PER_MM,
  FeedBlock,
  ImageBlock,
  PrintBlock,
  QrBlock,
  TextBlock,
  blocks,
  needsPreview,
  colsFor,
  deleteTemplate,
  fetchPrinterInfo,
  fetchTemplates,
  layoutDoc,
  lengthMm,
  loadTemplate,
  newBlock,
  previews,
  printDoc,
  printerInfo,
  refreshPreview,
  saveTemplate,
  schedulePreview,
  setBlocks,
  setLengthMm,
  templates,
  textLines,
  textSpacing,
} from '../print';

/** Preview scale: CSS px per printer dot (576 dots → ~403px paper). */
const DOT = 0.7;
/** Approximate monospace glyph aspect (width ≈ 0.6 × font-size). */
const MONO_ASPECT = 0.6;

const px = (dots: number) => `${(dots * DOT).toFixed(2)}px`;
const mm = (dots: number) => (dots / DOTS_PER_MM).toFixed(1);

export default function PrintPanel() {
  const [status, setStatus] = createSignal('');
  const [error, setError] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [tplName, setTplName] = createSignal('');
  const [tplPick, setTplPick] = createSignal('');

  onMount(() => {
    void fetchPrinterInfo().catch((e) => setError(String(e)));
    void fetchTemplates().catch(() => {});
    void fetchCoupons();
    // Previews are session-scoped blob URLs — refetch for a draft that came
    // back from a tab switch with revoked/missing images.
    for (const b of blocks()) {
      if (needsPreview(b) && !previews()[b._uid]?.url) {
        void refreshPreview(b);
      }
    }
  });

  async function run(label: string, fn: () => Promise<string | void>) {
    setError('');
    setStatus(`${label}…`);
    setBusy(true);
    try {
      const outcome = await fn();
      setStatus(outcome ?? `${label} done`);
    } catch (e) {
      setError(String(e));
      setStatus('');
    } finally {
      setBusy(false);
    }
  }

  const layout = createMemo(() => layoutDoc(blocks(), lengthMm(), printerInfo(), previews()));
  const bounded = () => lengthMm() !== null;

  function update(i: number, patch: Partial<PrintBlock>) {
    let changed: PrintBlock | undefined;
    setBlocks((bs) =>
      bs.map((b, j) => {
        if (j !== i) return b;
        changed = { ...b, ...patch } as PrintBlock;
        return changed;
      }),
    );
    if (changed && needsPreview(changed)) {
      schedulePreview(changed);
    }
  }

  function move(i: number, dir: -1 | 1) {
    setBlocks((bs) => {
      const j = i + dir;
      if (j < 0 || j >= bs.length) return bs;
      const next = bs.slice();
      [next[i], next[j]] = [next[j], next[i]];
      return next;
    });
  }

  function remove(i: number) {
    setBlocks((bs) => bs.filter((_, j) => j !== i));
  }

  function add(type: PrintBlock['type']) {
    setBlocks((bs) => [...bs, newBlock(type)]);
  }

  function onImageFile(i: number, file: File) {
    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = String(reader.result);
      const b64 = dataUrl.slice(dataUrl.indexOf(',') + 1);
      update(i, { data_b64: b64, _name: file.name } as Partial<ImageBlock>);
    };
    reader.readAsDataURL(file);
  }

  async function doPrint() {
    await run('print', async () => {
      const r = await printDoc();
      const kb = r.bytes.toLocaleString();
      if (r.status === 'printed') return `printed — ${kb} bytes`;
      if (r.status === 'mock') return `printer in mock mode — rendered only (${kb} bytes)`;
      return 'printer mode is off — nothing printed';
    });
  }

  return (
    <section class="persona-panel print-panel">
      <div class="panel-body">
        {/* Strip length mode */}
        <div class="field inline">
          <label>strip</label>
          <select
            value={bounded() ? 'fixed' : 'continuous'}
            onChange={(e) => setLengthMm(e.currentTarget.value === 'fixed' ? 50 : null)}
          >
            <option value="continuous">continuous (cut after content)</option>
            <option value="fixed">fixed length (exact cut-to-cut)</option>
          </select>
          <Show when={bounded()}>
            <input
              type="number"
              class="print-mm"
              min="20"
              max="500"
              step="1"
              value={lengthMm() ?? 50}
              onInput={(e) => {
                const v = Number(e.currentTarget.value);
                if (Number.isFinite(v)) setLengthMm(v);
              }}
            />
            <span class="muted small">mm cut-to-cut</span>
            <button class="mini" onClick={() => setLengthMm(50)} title="80×50mm plaque insert">
              50mm plaque
            </button>
          </Show>
          <span class="muted small">printer: {printerInfo().mode}</span>
        </div>

        {/* F4: how often a trial keepsake gets a "bad lawyer" coupon. Live. */}
        <div class="field inline">
          <label title="How often a trial receipt gets a Dewey, Soakem & Howe coupon">coupons</label>
          <select value={couponFrequency()} onChange={(e) => void setCoupons(e.currentTarget.value)}>
            <option value="off">off</option>
            <option value="rare">rare (~1 in 6)</option>
            <option value="sometimes">sometimes (~1 in 3)</option>
            <option value="always">always</option>
          </select>
          <span class="muted small">on trial keepsakes</span>
        </div>

        <div class="print-columns">
          {/* ---- editor column ---- */}
          <div class="print-editor">
            <div class="btn-row">
              <button onClick={() => add('text')}>+ Text</button>
              <button onClick={() => add('rule')}>+ Rule</button>
              <button onClick={() => add('feed')}>+ Feed</button>
              <button onClick={() => add('qr')}>+ QR</button>
              <button onClick={() => add('barcode')}>+ Barcode</button>
              <button onClick={() => add('image')}>+ Image</button>
              <button onClick={() => add('banner')}>+ Banner</button>
            </div>

            <Show when={blocks().length === 0}>
              <p class="muted small">Add blocks to compose a print. Everything previews on the right.</p>
            </Show>

            <Index each={blocks()}>
              {(b, i) => (
                <div class="print-block-card">
                  <div class="row-line print-block-head">
                    <span class="print-block-kind">{b().type}</span>
                    <span class="muted mini">
                      {mm(layout().heights[i] + layout().flexDots[i])}mm
                    </span>
                    <span class="print-block-tools">
                      <button class="mini" disabled={i === 0} onClick={() => move(i, -1)}>↑</button>
                      <button class="mini" disabled={i === blocks().length - 1} onClick={() => move(i, 1)}>↓</button>
                      <button class="mini danger" onClick={() => remove(i)}>✕</button>
                    </span>
                  </div>

                  <Show when={b().type === 'text'}>
                    {(_) => {
                      const t = () => b() as TextBlock;
                      return (
                        <>
                          <textarea
                            rows={2}
                            placeholder="text to print (ASCII; smart quotes folded)"
                            value={t().text}
                            onInput={(e) => update(i, { text: e.currentTarget.value })}
                          />
                          <div class="row-line">
                            <select value={t().align} onChange={(e) => update(i, { align: e.currentTarget.value as TextBlock['align'] })}>
                              <option value="left">left</option>
                              <option value="center">center</option>
                              <option value="right">right</option>
                            </select>
                            <select value={t().font} onChange={(e) => update(i, { font: e.currentTarget.value as TextBlock['font'] })}>
                              <option value="a">font A (48 col)</option>
                              <option value="b">font B (64 col)</option>
                            </select>
                            <label class="checkbox"><input type="checkbox" checked={t().bold} onChange={(e) => update(i, { bold: e.currentTarget.checked })} /> bold</label>
                            <label class="checkbox"><input type="checkbox" checked={t().underline} onChange={(e) => update(i, { underline: e.currentTarget.checked })} /> underline</label>
                            <label class="checkbox"><input type="checkbox" checked={t().inverse} onChange={(e) => update(i, { inverse: e.currentTarget.checked })} /> inverse</label>
                          </div>
                          <div class="row-line">
                            <label class="mini-label">w×</label>
                            <select value={t().size_w} onChange={(e) => update(i, { size_w: Number(e.currentTarget.value) })}>
                              {[1, 2, 3, 4, 5, 6, 7, 8].map((n) => <option value={n}>{n}</option>)}
                            </select>
                            <label class="mini-label">h×</label>
                            <select value={t().size_h} onChange={(e) => update(i, { size_h: Number(e.currentTarget.value) })}>
                              {[1, 2, 3, 4, 5, 6, 7, 8].map((n) => <option value={n}>{n}</option>)}
                            </select>
                          </div>
                        </>
                      );
                    }}
                  </Show>

                  <Show when={b().type === 'rule'}>
                    <div class="row-line">
                      <label class="checkbox">
                        <input
                          type="checkbox"
                          checked={(b() as RuleBlockT).heavy}
                          onChange={(e) => update(i, { heavy: e.currentTarget.checked })}
                        />{' '}
                        heavy (=)
                      </label>
                    </div>
                  </Show>

                  <Show when={b().type === 'feed'}>
                    {(_) => {
                      const f = () => b() as FeedBlock;
                      return (
                        <div class="row-line">
                          <label class="mini-label">lines</label>
                          <input
                            type="number"
                            min="1"
                            max="10"
                            value={f().lines}
                            disabled={bounded() && f().flex > 0}
                            onInput={(e) => update(i, { lines: Number(e.currentTarget.value) })}
                          />
                          <Show when={bounded()}>
                            <label class="mini-label" title="0 = fixed; ≥1 absorbs leftover strip length (spring)">flex</label>
                            <input
                              type="number"
                              min="0"
                              max="10"
                              value={f().flex}
                              onInput={(e) => update(i, { flex: Number(e.currentTarget.value) })}
                            />
                            <Show when={f().flex > 0}>
                              <span class="muted small">spring — grows to fill</span>
                            </Show>
                          </Show>
                        </div>
                      );
                    }}
                  </Show>

                  <Show when={b().type === 'qr'}>
                    {(_) => {
                      const q = () => b() as QrBlock;
                      return (
                        <div class="row-line">
                          <input
                            type="text"
                            placeholder="https://…"
                            value={q().data}
                            onInput={(e) => update(i, { data: e.currentTarget.value })}
                          />
                          <label class="mini-label">module</label>
                          <select value={q().module} onChange={(e) => update(i, { module: Number(e.currentTarget.value) })}>
                            {[2, 3, 4, 5, 6, 8, 10, 12].map((n) => <option value={n}>{n}</option>)}
                          </select>
                          <label class="mini-label">ecc</label>
                          <select value={q().ecc} onChange={(e) => update(i, { ecc: e.currentTarget.value as QrBlock['ecc'] })}>
                            <option value="l">L</option>
                            <option value="m">M</option>
                            <option value="q">Q</option>
                            <option value="h">H</option>
                          </select>
                        </div>
                      );
                    }}
                  </Show>

                  <Show when={b().type === 'barcode'}>
                    {(_) => {
                      const bc = () => b() as BarcodeBlock;
                      return (
                        <div class="row-line">
                          <input
                            type="text"
                            placeholder="data"
                            value={bc().data}
                            onInput={(e) => update(i, { data: e.currentTarget.value })}
                          />
                          <select value={bc().symbology} onChange={(e) => update(i, { symbology: e.currentTarget.value as BarcodeBlock['symbology'] })}>
                            <option value="code128">CODE128</option>
                            <option value="code39">CODE39</option>
                            <option value="ean13">EAN-13</option>
                            <option value="upca">UPC-A</option>
                          </select>
                          <label class="mini-label">h</label>
                          <input
                            type="number"
                            min="24"
                            max="200"
                            value={bc().height}
                            onInput={(e) => update(i, { height: Number(e.currentTarget.value) })}
                          />
                        </div>
                      );
                    }}
                  </Show>

                  <Show when={b().type === 'image'}>
                    {(_) => {
                      const im = () => b() as ImageBlock;
                      return (
                        <>
                          <div class="row-line">
                            <input
                              type="file"
                              accept="image/png,image/jpeg"
                              onChange={(e) => {
                                const f = e.currentTarget.files?.[0];
                                if (f) onImageFile(i, f);
                              }}
                            />
                            <Show when={im()._name}>
                              <span class="muted small">{im()._name}</span>
                            </Show>
                          </div>
                          <div class="row-line">
                            <label class="mini-label">dither</label>
                            <select value={im().dither} onChange={(e) => update(i, { dither: e.currentTarget.value as ImageBlock['dither'] })}>
                              <option value="fs">Floyd–Steinberg</option>
                              <option value="atkinson">Atkinson</option>
                              <option value="bayer">Bayer</option>
                              <option value="none">threshold</option>
                            </select>
                            <label class="mini-label">width %</label>
                            <input
                              type="number"
                              min="10"
                              max="100"
                              value={im().width_pct}
                              onInput={(e) => update(i, { width_pct: Number(e.currentTarget.value) })}
                            />
                            <Show when={bounded()}>
                              <label class="checkbox">
                                <input
                                  type="checkbox"
                                  checked={im().shrink}
                                  onChange={(e) => update(i, { shrink: e.currentTarget.checked })}
                                />{' '}
                                shrink to fit
                              </label>
                            </Show>
                          </div>
                          <div class="row-line">
                            <label class="mini-label" title="<1 brightens mid-tones; blank = printer default">gamma</label>
                            <input
                              type="number"
                              min="0.2"
                              max="4"
                              step="0.05"
                              placeholder={printerInfo().image_gamma.toFixed(2)}
                              value={im().gamma ?? ''}
                              onInput={(e) => {
                                const raw = e.currentTarget.value;
                                const v = Number(raw);
                                update(i, { gamma: raw === '' || !Number.isFinite(v) ? null : v });
                              }}
                            />
                            <label class="mini-label" title="+ lightens (luma offset); blank = printer default">brightness</label>
                            <input
                              type="number"
                              min="-128"
                              max="128"
                              step="5"
                              placeholder={String(printerInfo().image_brightness)}
                              value={im().brightness ?? ''}
                              onInput={(e) => {
                                const raw = e.currentTarget.value;
                                const v = Number(raw);
                                update(i, { brightness: raw === '' || !Number.isFinite(v) ? null : v });
                              }}
                            />
                            <label class="mini-label" title="<1 flattens (lifts shadows, tames highlights); blank = printer default">contrast</label>
                            <input
                              type="number"
                              min="0.2"
                              max="3"
                              step="0.05"
                              placeholder={printerInfo().image_contrast.toFixed(2)}
                              value={im().contrast ?? ''}
                              onInput={(e) => {
                                const raw = e.currentTarget.value;
                                const v = Number(raw);
                                update(i, { contrast: raw === '' || !Number.isFinite(v) ? null : v });
                              }}
                            />
                          </div>
                          <Show when={previews()[im()._uid]?.error}>
                            <span class="err">{previews()[im()._uid]?.error}</span>
                          </Show>
                        </>
                      );
                    }}
                  </Show>

                  <Show when={b().type === 'banner'}>
                    {(_) => {
                      const bn = () => b() as BannerBlock;
                      return (
                        <>
                          <div class="row-line">
                            <input
                              type="text"
                              placeholder="WET COURT"
                              maxlength="32"
                              value={bn().text}
                              onInput={(e) => update(i, { text: e.currentTarget.value })}
                            />
                            <label class="mini-label">style</label>
                            <select value={bn().style} onChange={(e) => update(i, { style: e.currentTarget.value as BannerBlock['style'] })}>
                              <option value="solid">solid</option>
                              <option value="outline">outline</option>
                              <option value="ascii">ascii art</option>
                            </select>
                            <label class="mini-label">height %</label>
                            <input
                              type="number"
                              min="20"
                              max="100"
                              step="5"
                              value={bn().height_pct}
                              onInput={(e) => update(i, { height_pct: Number(e.currentTarget.value) })}
                            />
                          </div>
                          <p class="muted small">
                            prints sideways down the tape — rotate the strip a quarter-turn
                            counter-clockwise (start of the strip on the left) to read it
                          </p>
                          <Show when={previews()[bn()._uid]?.error}>
                            <span class="err">{previews()[bn()._uid]?.error}</span>
                          </Show>
                        </>
                      );
                    }}
                  </Show>
                </div>
              )}
            </Index>

            {/* Templates */}
            <div class="field">
              <label>templates</label>
              <div class="row-line">
                <input
                  type="text"
                  placeholder="template name"
                  value={tplName()}
                  onInput={(e) => setTplName(e.currentTarget.value)}
                />
                <button
                  disabled={busy() || !tplName().trim() || blocks().length === 0}
                  onClick={() => {
                    const name = tplName().trim();
                    if (
                      templates().some((t) => t.name === name) &&
                      !confirm(`Overwrite template "${name}"?`)
                    ) {
                      return;
                    }
                    void run('save template', () => saveTemplate(name));
                  }}
                >
                  Save
                </button>
              </div>
              <Show when={templates().length > 0}>
                <div class="row-line">
                  <select value={tplPick()} onChange={(e) => setTplPick(e.currentTarget.value)}>
                    <option value="">— pick a template —</option>
                    <Index each={templates()}>
                      {(t) => <option value={t().name}>{t().name} ({t().blocks} blocks)</option>}
                    </Index>
                  </select>
                  <button
                    class="mini"
                    disabled={busy() || !tplPick()}
                    onClick={() =>
                      run('load template', async () => {
                        await loadTemplate(tplPick());
                        setTplName(tplPick());
                        for (const b of blocks()) {
                          if (needsPreview(b)) void refreshPreview(b);
                        }
                      })
                    }
                  >
                    Load
                  </button>
                  <button
                    class="mini danger"
                    disabled={busy() || !tplPick()}
                    onClick={() => {
                      if (confirm(`Delete template "${tplPick()}"?`)) {
                        void run('delete template', async () => {
                          await deleteTemplate(tplPick());
                          setTplPick('');
                        });
                      }
                    }}
                  >
                    Delete
                  </button>
                </div>
              </Show>
            </div>

            <div class="btn-row">
              <button
                class="print-go"
                disabled={busy() || blocks().length === 0 || layout().overflow > 0}
                onClick={() => void doPrint()}
              >
                PRINT
              </button>
              <Show when={layout().overflow > 0}>
                <span class="err">content overflows the strip by {mm(layout().overflow)}mm</span>
              </Show>
            </div>

            <div class="status-line">
              <Show when={status()}><span class="status">{status()}</span></Show>
              <Show when={error()}><span class="err">{error()}</span></Show>
            </div>
          </div>

          {/* ---- preview column ---- */}
          <div class="print-preview">
            <div class="muted small print-preview-caption">
              preview — {printerInfo().width_dots} dots ({mm(printerInfo().width_dots)}mm) wide
              {bounded()
                ? ` · ${lengthMm()}mm strip · content ${mm(layout().used)}/${mm(layout().budget ?? 0)}mm`
                : ` · ~${mm(layout().used)}mm of content`}
              <Show when={bounded()}> · barcode shown as placeholder</Show>
            </div>
            <div
              class="print-paper"
              style={{
                width: px(printerInfo().width_dots),
                height: bounded() ? px(Math.round((lengthMm() ?? 0) * DOTS_PER_MM)) : undefined,
              }}
            >
              <Show when={bounded()}>
                <div
                  class="print-dead-zone"
                  style={{ height: px(printerInfo().head_to_cutter_dots) }}
                  title={`unprintable head-to-cutter dead zone (${mm(printerInfo().head_to_cutter_dots)}mm)`}
                />
              </Show>
              <Index each={blocks()}>
                {(b, i) => <BlockPreviewView b={b()} i={i} layout={layout()} bounded={bounded()} />}
              </Index>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}

// Narrow re-exported type aliases (Index's callback needs concrete casts).
type RuleBlockT = Extract<PrintBlock, { type: 'rule' }>;

function BlockPreviewView(props: {
  b: PrintBlock;
  i: number;
  layout: ReturnType<typeof layoutDoc>;
  bounded: boolean;
}) {
  const info = printerInfo;
  const b = () => props.b;

  return (
    <>
      <Show when={b().type === 'text'}>
        {(_) => {
          const t = () => b() as TextBlock;
          return (
            <div
              class="print-text"
              style={{
                'text-align': t().align,
                'font-weight': t().bold ? '700' : '400',
                'text-decoration': t().underline ? 'underline' : 'none',
                'font-size': px((GLYPH_W_OF[t().font] * t().size_w) / MONO_ASPECT),
                'line-height': px(textSpacing(t())),
              }}
            >
              <Index each={textLines(t(), info().width_dots)}>
                {(line) => (
                  <div class={t().inverse ? 'print-inverse' : ''}>{line() || ' '}</div>
                )}
              </Index>
            </div>
          );
        }}
      </Show>

      <Show when={b().type === 'rule'}>
        <div class="print-text" style={{ 'line-height': px(30), 'font-size': px(12 / MONO_ASPECT) }}>
          {((b() as RuleBlockT).heavy ? '=' : '-').repeat(colsFor(printerInfo().width_dots, 'a'))}
        </div>
      </Show>

      <Show when={b().type === 'feed'}>
        {(_) => {
          const f = () => b() as FeedBlock;
          const h = () => props.layout.heights[props.i] + props.layout.flexDots[props.i];
          return (
            <div
              class={`print-feed ${props.bounded && f().flex > 0 ? 'flex' : ''}`}
              style={{ height: px(h()) }}
            >
              <Show when={props.bounded && f().flex > 0}>
                <span>⇕ flex ({mm(h())}mm)</span>
              </Show>
            </div>
          );
        }}
      </Show>

      <Show when={b().type === 'qr' || b().type === 'image' || b().type === 'banner'}>
        {(_) => {
          const pv = () => previews()[b()._uid];
          return (
            <div class="print-raster">
              <Show
                when={pv()?.url}
                fallback={
                  <div class="print-placeholder" style={{ width: px(120), height: px(120) }}>
                    {b().type === 'qr' ? 'QR' : b().type}
                  </div>
                }
              >
                <img src={pv()!.url} style={{ width: px(pv()!.w) }} alt="" />
              </Show>
            </div>
          );
        }}
      </Show>

      <Show when={b().type === 'barcode'}>
        {(_) => {
          const bc = () => b() as BarcodeBlock;
          return (
            <div class="print-raster">
              <div
                class="print-barcode"
                style={{ height: px(bc().height), width: px(printerInfo().width_dots * 0.6) }}
              />
              <Show when={!props.bounded}>
                <div class="print-hri">{bc().data || '· · ·'}</div>
              </Show>
            </div>
          );
        }}
      </Show>
    </>
  );
}

const GLYPH_W_OF: Record<'a' | 'b', number> = { a: 12, b: 9 };
