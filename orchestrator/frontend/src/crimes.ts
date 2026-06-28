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
  category_filter: string | null;
  queue: string[];
}

export const [crimes, setCrimes] = createSignal<Crime[]>([]);
export const [categories, setCategories] = createSignal<string[]>([]);
export const [categoryFilter, setCategoryFilter] = createSignal<string | null>(null);
export const [chargeQueue, setChargeQueue] = createSignal<string[]>([]);

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
  setCategoryFilter(data.category_filter);
  setChargeQueue(data.queue);
}

export async function fetchCrimes(): Promise<void> {
  const res = await fetch('/operator/crimes');
  if (!res.ok) throw new Error(await asError(res));
  apply((await res.json()) as CrimesResponse);
}

export async function addCrime(
  category: string,
  charge: string,
  subject?: string | null,
): Promise<void> {
  const res = await fetch('/operator/crimes', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ category, charge, subject: subject?.trim() || null }),
  });
  if (!res.ok) throw new Error(await asError(res));
  await fetchCrimes();
}

export async function updateCrime(c: Crime): Promise<void> {
  const res = await fetch(`/operator/crimes/${c.id}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(c),
  });
  if (!res.ok) throw new Error(await asError(res));
  await fetchCrimes();
}

export async function deleteCrime(id: number): Promise<void> {
  const res = await fetch(`/operator/crimes/${id}`, { method: 'DELETE' });
  if (!res.ok) throw new Error(await asError(res));
  await fetchCrimes();
}

export async function setCrimeFilter(category: string | null): Promise<void> {
  const res = await fetch('/operator/crimes/filter', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ category }),
  });
  if (!res.ok) throw new Error(await asError(res));
  await fetchCrimes();
}

export async function queueCharge(charge: string): Promise<void> {
  const res = await fetch('/operator/crimes/queue', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ charge }),
  });
  if (!res.ok) throw new Error(await asError(res));
  await fetchCrimes();
}

export async function unqueueCharge(index: number): Promise<void> {
  const res = await fetch(`/operator/crimes/queue/${index}`, { method: 'DELETE' });
  if (!res.ok) throw new Error(await asError(res));
  await fetchCrimes();
}
