// Minimal ambient typings for @met4citizen/talkinghead@^1.7.0. The package
// ships only `modules/talkinghead.mjs`, no .d.ts. Cover the surface we use
// in face.ts; let everything else fall through as any.
declare module '@met4citizen/talkinghead' {
  // Internal per-morph-target state. `realtime` is the unsmoothed channel
  // used by streamAudio's viseme path; we write it directly for amplitude-
  // driven jaw motion because the public setFixedValue setter goes through
  // exponential smoothing that's too slow for ~30 Hz reactivity.
  export interface MorphTargetState {
    fixed: number | null;
    realtime: number | null;
    system: number | null;
    baseline: number | null;
    value: number;
    applied: number;
    needsUpdate: boolean;
  }

  export class TalkingHead {
    mtAvatar: Record<string, MorphTargetState>;
    constructor(node: HTMLElement, opt?: Record<string, unknown>);
    showAvatar(opts: {
      url: string;
      body?: 'M' | 'F';
      avatarMood?: string;
      lipsyncLang?: string;
    }, onprogress?: (p: ProgressEvent) => void): Promise<void>;
    setMood(s: string): void;
    setFixedValue(mt: string, val: number | null, ms?: number | null): void;
    setBaselineValue(mt: string, val: number | null): void;
    getMorphTargetNames(): string[];
    getMoodNames(): string[];
    stop(): void;
    stopSpeaking(): void;
  }
}
