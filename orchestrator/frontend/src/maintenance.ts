import { createSignal } from 'solid-js';

// Mirrors the backend `Role` wire names (src/hardware/maintenance.rs).
export type Role = 'ai_judge' | 'gavel' | 'turret';

export interface ServoCal {
  min: number;
  max: number;
  center: number;
  invert: boolean;
  limit_min_deg: number;
  limit_max_deg: number;
}

export interface Calibration {
  role: string;
  pan?: ServoCal | null;
  tilt?: ServoCal | null;
  fire_presets_ms: number[];
}

export interface DeviceInfo {
  role: string;
  addr: string;
}

// Mirrors `HwAckResult` (serde tag = "result").
export type AckResult =
  | { result: 'ok'; line: string }
  | { result: 'err'; reason: string }
  | { result: 'timeout' }
  | { result: 'no_device' };

// A direct command as the REST body expects it (the `cmd` discriminant plus
// its args, flattened alongside `target`/`stream`).
export type CmdSpec =
  | { cmd: 'fire'; ms: number }
  | { cmd: 'gavel' }
  | { cmd: 'aim'; pan: number; tilt: number }
  | { cmd: 'panel'; pattern: 'idle' | 'thinking' | 'verdict' }
  | { cmd: 'ping' };

// True while the FSM is in maintenance mode. Driven by the `maintenance`
// broadcast (see ws.ts) and set optimistically by enter/exit.
export const [maintenanceActive, setMaintenanceActive] = createSignal(false);
export const [devices, setDevices] = createSignal<DeviceInfo[]>([]);
export const [calibrations, setCalibrations] = createSignal<Record<string, Calibration>>({});

/** Whether a device role currently has a live connection. */
export function deviceConnected(role: Role): boolean {
  return devices().some((d) => d.role === role);
}

// --- ws.ts hooks: keep the presence list in sync with broadcast events ---
export function onDeviceConnected(role: string, addr: string): void {
  setDevices((prev) => [...prev.filter((d) => d.role !== role), { role, addr }]);
}
export function onDeviceDisconnected(role: string): void {
  setDevices((prev) => prev.filter((d) => d.role !== role));
}

async function asError(res: Response): Promise<string> {
  try {
    const t = await res.text();
    return t || `${res.status} ${res.statusText}`;
  } catch {
    return `${res.status} ${res.statusText}`;
  }
}

export async function enterMaintenance(): Promise<void> {
  const res = await fetch('/maintenance/enter', { method: 'POST' });
  if (!res.ok) throw new Error(await asError(res));
  setMaintenanceActive(true); // confirmed by the `maintenance` broadcast
}

export async function exitMaintenance(): Promise<void> {
  const res = await fetch('/maintenance/exit', { method: 'POST' });
  if (!res.ok) throw new Error(await asError(res));
  setMaintenanceActive(false);
}

export async function fetchDevices(): Promise<void> {
  try {
    const res = await fetch('/maintenance/devices');
    if (!res.ok) return;
    setDevices((await res.json()) as DeviceInfo[]);
  } catch {
    /* non-fatal — presence also arrives via broadcast */
  }
}

export async function fetchCalibrations(): Promise<void> {
  const res = await fetch('/maintenance/calibration');
  if (!res.ok) throw new Error(await asError(res));
  const list = (await res.json()) as Calibration[];
  const map: Record<string, Calibration> = {};
  for (const c of list) map[c.role] = c;
  setCalibrations(map);
}

export async function updateCalibration(role: string, cal: Calibration): Promise<Calibration> {
  const res = await fetch(`/maintenance/calibration/${encodeURIComponent(role)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(cal),
  });
  if (!res.ok) throw new Error(await asError(res));
  const updated = (await res.json()) as Calibration;
  setCalibrations((prev) => ({ ...prev, [role]: updated }));
  return updated;
}

export async function saveCalibration(role: string): Promise<void> {
  const res = await fetch(`/maintenance/calibration/${encodeURIComponent(role)}/save`, {
    method: 'POST',
  });
  if (!res.ok) throw new Error(await asError(res));
}

/**
 * Send one direct hardware command. With `stream: true` (the high-rate AIM
 * path) the server replies 202 and we return null; otherwise we return the
 * device's OK/ERR/timeout ack.
 */
export async function sendCommand(
  target: Role,
  spec: CmdSpec,
  stream = false,
): Promise<AckResult | null> {
  const res = await fetch('/maintenance/command', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ target, ...spec, stream }),
  });
  if (res.status === 202) return null;
  if (!res.ok) return { result: 'err', reason: await asError(res) };
  return (await res.json()) as AckResult;
}
