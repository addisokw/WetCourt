import { createSignal } from 'solid-js';

// Geometry constants — MUST mirror orchestrator/src/printer/custom.rs so the
// preview's height ledger matches what the renderer will do.
export const DOTS_PER_MM = 203 / 25.4;
export const FEED_LINE_DOTS = 30;
export const TEXT_LEADING_DOTS = 6;
const GLYPH_H: Record<Font, number> = { a: 24, b: 17 };
const GLYPH_W: Record<Font, number> = { a: 12, b: 9 };

export type Align = 'left' | 'center' | 'right';
export type Font = 'a' | 'b';
export type Ecc = 'l' | 'm' | 'q' | 'h';
export type Symbology = 'code128' | 'code39' | 'ean13' | 'upca';
export type DitherKind = 'fs' | 'atkinson' | 'bayer' | 'none';

// Client-only fields are prefixed with `_` and stripped before POSTing.
export interface TextBlock {
  type: 'text';
  text: string;
  align: Align;
  bold: boolean;
  underline: boolean;
  inverse: boolean;
  font: Font;
  size_w: number;
  size_h: number;
  _uid: string;
}
export interface RuleBlock {
  type: 'rule';
  heavy: boolean;
  _uid: string;
}
export interface FeedBlock {
  type: 'feed';
  lines: number;
  /** Bounded mode: 0 = fixed, >=1 = spring weight absorbing leftover length. */
  flex: number;
  _uid: string;
}
export interface QrBlock {
  type: 'qr';
  data: string;
  module: number;
  ecc: Ecc;
  _uid: string;
}
export interface BarcodeBlock {
  type: 'barcode';
  data: string;
  symbology: Symbology;
  height: number;
  width: number;
  _uid: string;
}
export interface ImageBlock {
  type: 'image';
  data_b64: string;
  dither: DitherKind;
  width_pct: number;
  shrink: boolean;
  _name?: string;
  _uid: string;
}
export type PrintBlock = TextBlock | RuleBlock | FeedBlock | QrBlock | BarcodeBlock | ImageBlock;

export interface TemplateMeta {
  name: string;
  blocks: number;
}

export interface PrinterInfo {
  mode: string;
  width_dots: number;
  head_to_cutter_dots: number;
}

export interface PrintResult {
  status: 'printed' | 'mock' | 'off';
  bytes: number;
}

/** Server-rendered preview raster for a QR/image block (pixels = dots). */
export interface BlockPreview {
  url: string;
  w: number;
  h: number;
  error?: string;
}

let nextUid = 1;
export function uid(): string {
  return `b${nextUid++}`;
}

// Module-scope state so the draft survives tab switches (the panel unmounts).
export const [blocks, setBlocks] = createSignal<PrintBlock[]>([]);
export const [lengthMm, setLengthMm] = createSignal<number | null>(null);
export const [templates, setTemplates] = createSignal<TemplateMeta[]>([]);
export const [previews, setPreviews] = createSignal<Record<string, BlockPreview>>({});
export const [printerInfo, setPrinterInfo] = createSignal<PrinterInfo>({
  mode: 'mock',
  width_dots: 576,
  head_to_cutter_dots: 136,
});

export function newBlock(type: PrintBlock['type']): PrintBlock {
  const _uid = uid();
  switch (type) {
    case 'text':
      return { type, text: '', align: 'left', bold: false, underline: false, inverse: false, font: 'a', size_w: 1, size_h: 1, _uid };
    case 'rule':
      return { type, heavy: false, _uid };
    case 'feed':
      return { type, lines: 1, flex: 0, _uid };
    case 'qr':
      return { type, data: '', module: 6, ecc: 'm', _uid };
    case 'barcode':
      return { type, data: '', symbology: 'code128', height: 80, width: 3, _uid };
    case 'image':
      return { type, data_b64: '', dither: 'fs', width_pct: 100, shrink: false, _uid };
  }
}

// ---- text metrics (ports of thermal-printer's text.rs + report.rs asciify) ----

export function asciify(s: string): string {
  return s
    .replace(/…/g, '...')
    .replace(/[‘’‚′]/g, "'")
    .replace(/[“”„″]/g, '"')
    .replace(/[–—−]/g, '-')
    .replace(/ /g, ' ')
    .replace(/[^\x00-\x7F]/g, ' ');
}

export function colsFor(widthDots: number, font: Font): number {
  return Math.max(1, Math.floor(widthDots / GLYPH_W[font]));
}

/** Word-wrap to `cols`, hard-splitting oversize words — mirrors text.rs::wrap. */
export function wrap(s: string, cols: number): string[] {
  cols = Math.max(1, cols);
  const lines: string[] = [];
  let cur = '';
  for (const word of s.split(/\s+/).filter((w) => w.length > 0)) {
    if (word.length > cols) {
      if (cur) {
        lines.push(cur);
        cur = '';
      }
      let w = word;
      while (w.length > cols) {
        lines.push(w.slice(0, cols));
        w = w.slice(cols);
      }
      cur = w;
    } else if (!cur) {
      cur = word;
    } else if (cur.length + 1 + word.length <= cols) {
      cur += ' ' + word;
    } else {
      lines.push(cur);
      cur = word;
    }
  }
  if (cur) lines.push(cur);
  if (lines.length === 0) lines.push('');
  return lines;
}

/** The wrapped preview lines of a text block (what will actually print). */
export function textLines(b: TextBlock, widthDots: number): string[] {
  const cols = Math.max(1, Math.floor(colsFor(widthDots, b.font) / b.size_w));
  const out: string[] = [];
  for (const raw of b.text.split('\n')) {
    out.push(...wrap(asciify(raw), cols));
  }
  if (out.length === 0) out.push('');
  return out;
}

export function textSpacing(b: TextBlock): number {
  return GLYPH_H[b.font] * b.size_h + TEXT_LEADING_DOTS;
}

// ---- height model (mirrors custom.rs Prep::height) ----

/** Exact height in dots; QR/image use the server preview raster (0 until loaded). */
export function blockHeight(
  b: PrintBlock,
  bounded: boolean,
  widthDots: number,
  preview: BlockPreview | undefined,
): number {
  switch (b.type) {
    case 'text':
      return textLines(b, widthDots).length * textSpacing(b);
    case 'rule':
      return GLYPH_H.a + TEXT_LEADING_DOTS;
    case 'feed':
      return bounded && b.flex > 0 ? 0 : b.lines * FEED_LINE_DOTS;
    case 'qr':
    case 'image':
      return preview?.h ?? 0;
    case 'barcode':
      return b.height + (bounded ? 0 : FEED_LINE_DOTS);
  }
}

export interface DocLayout {
  /** Fixed height per block (flex feeds at 0), parallel to blocks. */
  heights: number[];
  /** Flex dots granted per block (0 for non-springs), parallel to blocks. */
  flexDots: number[];
  used: number;
  /** Bounded only: printable budget (length − dead zone). */
  budget: number | null;
  overflow: number;
  /** Bounded only: trailing fill before the cut. */
  fill: number;
}

export function layoutDoc(
  bs: PrintBlock[],
  mm: number | null,
  info: PrinterInfo,
  pv: Record<string, BlockPreview>,
): DocLayout {
  const bounded = mm !== null;
  const heights = bs.map((b) => blockHeight(b, bounded, info.width_dots, pv[b._uid]));
  const flexDots = bs.map(() => 0);
  const fixed = heights.reduce((a, h) => a + h, 0);
  if (!bounded) {
    return { heights, flexDots, used: fixed, budget: null, overflow: 0, fill: 0 };
  }
  const lengthDots = Math.round(mm * DOTS_PER_MM);
  const budget = lengthDots - info.head_to_cutter_dots;
  const leftover = budget - fixed;
  if (leftover > 0) {
    const springs = bs
      .map((b, i) => ({ b, i }))
      .filter(({ b }) => b.type === 'feed' && b.flex > 0);
    const totalWeight = springs.reduce((a, { b }) => a + (b as FeedBlock).flex, 0);
    if (totalWeight > 0) {
      let given = 0;
      springs.forEach(({ b, i }, idx) => {
        let share = Math.floor((leftover * (b as FeedBlock).flex) / totalWeight);
        if (idx === springs.length - 1) share = leftover - given;
        flexDots[i] = share;
        given += share;
      });
    }
  }
  const used = fixed + flexDots.reduce((a, d) => a + d, 0);
  return {
    heights,
    flexDots,
    used,
    budget,
    overflow: Math.max(0, fixed - budget),
    fill: Math.max(0, lengthDots - used),
  };
}

// ---- API ----

async function asError(res: Response): Promise<string> {
  try {
    const t = await res.text();
    return t || `${res.status} ${res.statusText}`;
  } catch {
    return `${res.status} ${res.statusText}`;
  }
}

/** The wire form: client-only `_` fields stripped, length attached. */
export function toDoc(bs: PrintBlock[], mm: number | null): unknown {
  return {
    blocks: bs.map((b) =>
      Object.fromEntries(Object.entries(b).filter(([k]) => !k.startsWith('_'))),
    ),
    length_mm: mm,
  };
}

export async function fetchPrinterInfo(): Promise<void> {
  const res = await fetch('/operator/print/config');
  if (!res.ok) throw new Error(await asError(res));
  setPrinterInfo((await res.json()) as PrinterInfo);
}

export async function printDoc(): Promise<PrintResult> {
  const res = await fetch('/operator/print', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(toDoc(blocks(), lengthMm())),
  });
  if (!res.ok) throw new Error(await asError(res));
  return (await res.json()) as PrintResult;
}

export async function fetchTemplates(): Promise<void> {
  const res = await fetch('/operator/print/templates');
  if (!res.ok) throw new Error(await asError(res));
  setTemplates((await res.json()) as TemplateMeta[]);
}

export async function saveTemplate(name: string): Promise<void> {
  const res = await fetch(`/operator/print/templates/${encodeURIComponent(name)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(toDoc(blocks(), lengthMm())),
  });
  if (!res.ok) throw new Error(await asError(res));
  await fetchTemplates();
}

export async function loadTemplate(name: string): Promise<void> {
  const res = await fetch(`/operator/print/templates/${encodeURIComponent(name)}`);
  if (!res.ok) throw new Error(await asError(res));
  const doc = (await res.json()) as { blocks: Omit<PrintBlock, '_uid'>[]; length_mm: number | null };
  setPreviews({});
  setBlocks(doc.blocks.map((b) => ({ ...b, _uid: uid() }) as PrintBlock));
  setLengthMm(doc.length_mm ?? null);
}

export async function deleteTemplate(name: string): Promise<void> {
  const res = await fetch(`/operator/print/templates/${encodeURIComponent(name)}`, {
    method: 'DELETE',
  });
  if (!res.ok) throw new Error(await asError(res));
  await fetchTemplates();
}

// ---- server-rendered previews (pixel-exact QR + dithered images) ----

async function fetchPreviewPng(path: string, body: unknown): Promise<BlockPreview> {
  const res = await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(await asError(res));
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  const img = new Image();
  img.src = url;
  await img.decode();
  return { url, w: img.naturalWidth, h: img.naturalHeight };
}

function setPreview(uidKey: string, p: BlockPreview) {
  setPreviews((prev) => {
    const old = prev[uidKey];
    if (old?.url && old.url !== p.url) URL.revokeObjectURL(old.url);
    return { ...prev, [uidKey]: p };
  });
}

const previewTimers: Record<string, number> = {};

/** Debounced server round-trip for a QR/image block's exact raster. */
export function schedulePreview(b: QrBlock | ImageBlock): void {
  clearTimeout(previewTimers[b._uid]);
  previewTimers[b._uid] = window.setTimeout(() => {
    void refreshPreview(b);
  }, 300);
}

export async function refreshPreview(b: QrBlock | ImageBlock): Promise<void> {
  try {
    if (b.type === 'qr') {
      if (!b.data) return;
      setPreview(b._uid, await fetchPreviewPng('/operator/print/preview_qr', {
        data: asciify(b.data),
        module: b.module,
        ecc: b.ecc,
      }));
    } else {
      if (!b.data_b64) return;
      setPreview(b._uid, await fetchPreviewPng('/operator/print/preview_image', {
        data_b64: b.data_b64,
        dither: b.dither,
        width_pct: b.width_pct,
      }));
    }
  } catch (e) {
    setPreviews((prev) => ({ ...prev, [b._uid]: { url: '', w: 0, h: 0, error: String(e) } }));
  }
}
