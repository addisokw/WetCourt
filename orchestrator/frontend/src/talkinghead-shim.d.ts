// Minimal ambient typings for @met4citizen/talkinghead@^1.7.0. The package
// ships only `modules/talkinghead.mjs`, no .d.ts. Cover the surface we use
// in face.ts; let everything else fall through as any.
declare module '@met4citizen/talkinghead' {
  export class TalkingHead {
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
