import { createSignal } from 'solid-js';

export interface Crime {
  id: number;
  category: string;
  charge: string;
  /** Optional tag for who/what the charge is about (e.g. creator name). */
  subject?: string | null;
  enabled: boolean;
}

export interface CrimesResponse {
  crimes: Crime[];
  categories: string[];
}

export const [crimes, setCrimes] = createSignal<Crime[]>([]);
export const [categories, setCategories] = createSignal<string[]>([]);

// Validation mirrors crimes-core::Crime::validate so the UI gives the same
// verdict the server would, before a round-trip.
export function validateCharge(charge: string): string | null {
  const len = charge.trim().length;
  if (len < 10) return `charge too short (${len}/10 min)`;
  if (len > 300) return `charge too long (${len}/300 max)`;
  return null;
}

export function validateCategory(category: string): string | null {
  const len = category.trim().length;
  if (len < 1 || len > 40) return 'category must be 1–40 chars';
  return null;
}

export function validateSubject(subject: string): string | null {
  const len = subject.trim().length;
  if (len === 0) return null; // optional
  if (len > 60) return 'subject must be 1–60 chars';
  return null;
}

async function asError(res: Response): Promise<string> {
  try {
    const t = await res.text();
    return t || `${res.status} ${res.statusText}`;
  } catch {
    return `${res.status} ${res.statusText}`;
  }
}

function apply(data: CrimesResponse) {
  setCrimes(data.crimes);
  setCategories(data.categories);
}

export async function fetchCrimes(): Promise<void> {
  const res = await fetch('/api/crimes');
  if (!res.ok) throw new Error(await asError(res));
  apply((await res.json()) as CrimesResponse);
}

// Low-level POST/PUT that return the saved crime WITHOUT refetching the whole
// list — used by CSV import so a bulk run does a single refetch at the end.
export async function postCrime(
  category: string,
  charge: string,
  subject?: string | null,
): Promise<Crime> {
  const res = await fetch('/api/crimes', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ category, charge, subject: subject?.trim() || null }),
  });
  if (!res.ok) throw new Error(await asError(res));
  return (await res.json()) as Crime;
}

export async function putCrime(c: Crime): Promise<Crime> {
  const res = await fetch(`/api/crimes/${c.id}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(c),
  });
  if (!res.ok) throw new Error(await asError(res));
  return (await res.json()) as Crime;
}

// Interactive single-edit paths refetch so the UI reflects server truth.
export async function addCrime(
  category: string,
  charge: string,
  subject?: string | null,
): Promise<void> {
  await postCrime(category, charge, subject);
  await fetchCrimes();
}

export async function updateCrime(c: Crime): Promise<void> {
  await putCrime(c);
  await fetchCrimes();
}

export async function deleteCrime(id: number): Promise<void> {
  const res = await fetch(`/api/crimes/${id}`, { method: 'DELETE' });
  if (!res.ok) throw new Error(await asError(res));
  await fetchCrimes();
}
