import { createSignal } from 'solid-js';
import { ROBOT_DEFAULTS, ROBOT_FIELDS, RobotParams } from './robotParams';

export interface Persona {
  id: string;
  display_name: string;
  system_prompt: string;
  guilty_bias: number;
  tts_voice: string;
  tts_speed: number | null;
  robot: RobotParams;
}

export interface PersonaSummary {
  id: string;
  display_name: string;
}

export interface PersonasResponse {
  active_id: string;
  personas: PersonaSummary[];
}

export interface TestResult {
  deliberation: string;
  guilty: boolean;
}

export const ID_RE = /^[a-z0-9_]+$/;

export function validatePersona(p: Persona): Record<string, string> {
  const errs: Record<string, string> = {};
  if (!ID_RE.test(p.id) || p.id.length < 1 || p.id.length > 32) {
    errs.id = 'id must match ^[a-z0-9_]+$, 1–32 chars';
  }
  if (!p.display_name || p.display_name.trim().length === 0) {
    errs.display_name = 'display_name required';
  }
  if (!p.system_prompt || p.system_prompt.length === 0) {
    errs.system_prompt = 'system_prompt required';
  } else if (p.system_prompt.length > 8000) {
    errs.system_prompt = `system_prompt too long (${p.system_prompt.length}/8000)`;
  }
  if (!(p.guilty_bias >= 0 && p.guilty_bias <= 1)) {
    errs.guilty_bias = 'guilty_bias must be 0.0–1.0';
  }
  if (!p.tts_voice || p.tts_voice.trim().length === 0) {
    errs.tts_voice = 'tts_voice required';
  }
  if (p.tts_speed !== null) {
    if (!(p.tts_speed >= 0.5 && p.tts_speed <= 2.0)) {
      errs.tts_speed = 'tts_speed must be 0.5–2.0 or default';
    }
  }
  for (const f of ROBOT_FIELDS) {
    const v = p.robot?.[f.key];
    if (!(typeof v === 'number' && v >= f.min && v <= f.max)) {
      errs[`robot.${f.key}`] = `${f.label} must be ${f.min}–${f.max}`;
    }
  }
  return errs;
}

export const [personas, setPersonas] = createSignal<PersonaSummary[]>([]);
export const [activeId, setActiveId] = createSignal<string>('');

async function asError(res: Response): Promise<string> {
  try {
    const t = await res.text();
    return t || `${res.status} ${res.statusText}`;
  } catch {
    return `${res.status} ${res.statusText}`;
  }
}

export async function fetchPersonas(): Promise<PersonasResponse> {
  const res = await fetch('/operator/personas');
  if (!res.ok) throw new Error(await asError(res));
  const data = (await res.json()) as PersonasResponse;
  setPersonas(data.personas);
  setActiveId(data.active_id);
  return data;
}

export async function fetchActivePersona(): Promise<Persona> {
  const res = await fetch('/operator/persona');
  if (!res.ok) throw new Error(await asError(res));
  return normalize((await res.json()) as Persona);
}

// Backend omits `tts_speed` when None; JSON parses it as undefined, but the
// UI checks `=== null`. Coerce here so the rest of the code can rely on null.
// Also backfill robot params defensively (the backend always sends them).
function normalize(p: Persona): Persona {
  return { ...p, tts_speed: p.tts_speed ?? null, robot: { ...ROBOT_DEFAULTS, ...(p.robot ?? {}) } };
}

export async function selectPersona(id: string): Promise<void> {
  const res = await fetch(`/operator/persona/${encodeURIComponent(id)}/select`, { method: 'POST' });
  if (!res.ok) throw new Error(await asError(res));
  setActiveId(id);
}

export async function applyPersona(p: Persona): Promise<Persona> {
  const res = await fetch(`/operator/persona/${encodeURIComponent(p.id)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(p),
  });
  if (!res.ok) throw new Error(await asError(res));
  return normalize((await res.json()) as Persona);
}

export async function savePersona(id: string): Promise<void> {
  const res = await fetch(`/operator/persona/${encodeURIComponent(id)}/save`, { method: 'POST' });
  if (!res.ok) throw new Error(await asError(res));
}

export async function createPersona(p: Persona): Promise<Persona> {
  const res = await fetch('/operator/persona', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(p),
  });
  if (!res.ok) throw new Error(await asError(res));
  return normalize((await res.json()) as Persona);
}

export async function testPersona(p: Persona, charge: string, plea: string): Promise<TestResult> {
  // Send the in-memory state by applying first, then testing. Simpler: test endpoint
  // uses persisted/in-memory state on backend, and Apply has already been done by
  // caller if desired. We just hit the test endpoint with charge/plea.
  const res = await fetch(`/operator/persona/${encodeURIComponent(p.id)}/test`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ charge, plea }),
  });
  if (!res.ok) throw new Error(await asError(res));
  return (await res.json()) as TestResult;
}
