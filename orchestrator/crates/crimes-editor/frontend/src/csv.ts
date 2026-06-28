// CSV import/export for the crimes list. Columns: id, category, subject, charge,
// enabled. Export round-trips back through import: rows with a known id update
// that crime, rows without (or with an unknown id) are added.
import type { Crime } from './crimes';

export interface ImportRow {
  id?: number;
  category: string;
  charge: string;
  subject: string | null;
  enabled?: boolean;
}

const NEEDS_QUOTE = /[",\r\n]/;
function esc(v: unknown): string {
  const s = v == null ? '' : String(v);
  return NEEDS_QUOTE.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

export function crimesToCsv(crimes: Crime[]): string {
  const header = ['id', 'category', 'subject', 'charge', 'enabled'];
  const lines = crimes.map((c) =>
    [c.id, c.category, c.subject ?? '', c.charge, c.enabled].map(esc).join(','),
  );
  return [header.join(','), ...lines].join('\r\n');
}

// Minimal RFC 4180 parser: handles quoted fields, embedded commas/newlines, and
// doubled "" escapes.
function parseRows(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let field = '';
  let inQuotes = false;
  let i = 0;
  const endField = () => {
    row.push(field);
    field = '';
  };
  const endRow = () => {
    endField();
    rows.push(row);
    row = [];
  };
  while (i < text.length) {
    const ch = text[i];
    if (inQuotes) {
      if (ch === '"') {
        if (text[i + 1] === '"') {
          field += '"';
          i += 2;
          continue;
        }
        inQuotes = false;
        i++;
        continue;
      }
      field += ch;
      i++;
      continue;
    }
    if (ch === '"') {
      inQuotes = true;
      i++;
    } else if (ch === ',') {
      endField();
      i++;
    } else if (ch === '\r') {
      i++;
    } else if (ch === '\n') {
      endRow();
      i++;
    } else {
      field += ch;
      i++;
    }
  }
  if (field.length > 0 || row.length > 0) endRow();
  return rows;
}

export function parseCrimesCsv(text: string): { rows: ImportRow[]; errors: string[] } {
  const errors: string[] = [];
  const raw = parseRows(text).filter((r) => r.some((c) => c.trim() !== ''));
  if (raw.length === 0) return { rows: [], errors: ['file is empty'] };

  const header = raw[0].map((h) => h.trim().toLowerCase());
  const col = (name: string) => header.indexOf(name);
  const ci = {
    id: col('id'),
    category: col('category'),
    charge: col('charge'),
    subject: col('subject'),
    enabled: col('enabled'),
  };
  if (ci.category < 0 || ci.charge < 0) {
    return { rows: [], errors: ['CSV needs at least "category" and "charge" columns'] };
  }

  const rows: ImportRow[] = [];
  for (let r = 1; r < raw.length; r++) {
    const cells = raw[r];
    const get = (idx: number) => (idx >= 0 ? (cells[idx] ?? '').trim() : '');

    const idStr = get(ci.id);
    let id: number | undefined;
    if (idStr) {
      const n = Number(idStr);
      if (!Number.isInteger(n) || n <= 0) {
        errors.push(`row ${r + 1}: invalid id "${idStr}"`);
        continue;
      }
      id = n;
    }

    const enabledStr = get(ci.enabled).toLowerCase();
    const enabled = enabledStr ? !['false', '0', 'no', 'off'].includes(enabledStr) : undefined;

    rows.push({
      id,
      category: get(ci.category),
      charge: get(ci.charge),
      subject: get(ci.subject) || null,
      enabled,
    });
  }
  return { rows, errors };
}
