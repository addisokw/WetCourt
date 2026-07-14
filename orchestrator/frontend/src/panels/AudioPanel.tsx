import { createSignal, onCleanup, Show } from 'solid-js';
import { getPlaybackCtx, startRecording, stopRecording } from '../audio';

// Audio check: verify the console's sound I/O before the doors open, without
// running a trial. Output has two rungs (local beep = browser→speakers only;
// TTS test = Kokoro→WS→robot-voice graph→speakers, the full trial path) and
// input has two rungs (live level meter = mic capture only; record &
// transcribe = mic→upload→Parakeet round trip). Server-side tests only run
// while the trial FSM is idle, so this can't talk over a live show. The
// recorder is shared with the plea path — another reason this is an
// idle-time tool.
export default function AudioPanel() {
  // ---- output ----
  const [ttsText, setTtsText] = createSignal('');
  const [ttsBusy, setTtsBusy] = createSignal(false);
  const [ttsStatus, setTtsStatus] = createSignal('');
  const [ttsError, setTtsError] = createSignal('');

  // ---- input ----
  const [meterOn, setMeterOn] = createSignal(false);
  const [micLevel, setMicLevel] = createSignal(0);
  const [micPeak, setMicPeak] = createSignal(0);
  const [micError, setMicError] = createSignal('');
  const [rec, setRec] = createSignal<'idle' | 'recording' | 'transcribing'>('idle');
  const [transcript, setTranscript] = createSignal('');
  const [sttError, setSttError] = createSignal('');

  /** Speakers, rung 1: a plain beep straight to the output device — no
   * backend, no robot chain. If this is silent, the problem is the browser's
   * output device, not the court. */
  function playTone() {
    const ctx = getPlaybackCtx();
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.connect(gain);
    gain.connect(ctx.destination);
    const t = ctx.currentTime;
    osc.frequency.setValueAtTime(660, t);
    osc.frequency.setValueAtTime(880, t + 0.18);
    gain.gain.setValueAtTime(0, t);
    gain.gain.linearRampToValueAtTime(0.4, t + 0.02);
    gain.gain.setValueAtTime(0.4, t + 0.32);
    gain.gain.linearRampToValueAtTime(0, t + 0.38);
    osc.start(t);
    osc.stop(t + 0.4);
  }

  /** Speakers, rung 2: the backend synthesizes a phrase in the active judge
   * voice and streams it down the normal TTS path. Playback arrives via the
   * operator WS like any trial speech. */
  async function runTtsTest() {
    setTtsError('');
    setTtsStatus('synthesizing…');
    setTtsBusy(true);
    try {
      const text = ttsText().trim();
      const res = await fetch('/operator/audio/tts_test', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(text ? { text } : {}),
      });
      if (!res.ok) {
        setTtsError(await res.text());
        setTtsStatus('');
      } else {
        setTtsStatus('stream started — you should hear the judge voice now');
      }
    } catch (e) {
      setTtsError(String(e));
      setTtsStatus('');
    } finally {
      setTtsBusy(false);
    }
  }

  // ---- live mic meter ----
  let meterStream: MediaStream | null = null;
  let meterCtx: AudioContext | null = null;
  let raf = 0;

  function stopMeter() {
    cancelAnimationFrame(raf);
    meterStream?.getTracks().forEach((t) => t.stop());
    void meterCtx?.close();
    meterStream = null;
    meterCtx = null;
    setMeterOn(false);
    setMicLevel(0);
  }

  /** Mic, rung 1: open the mic and show a live level bar. Proves capture
   * device + permission without touching the backend. */
  async function toggleMeter() {
    if (meterOn()) {
      stopMeter();
      return;
    }
    setMicError('');
    setMicPeak(0);
    try {
      meterStream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, channelCount: 1 },
      });
      meterCtx = new AudioContext();
      const analyser = meterCtx.createAnalyser();
      analyser.fftSize = 2048;
      meterCtx.createMediaStreamSource(meterStream).connect(analyser);
      const buf = new Float32Array(analyser.fftSize);
      setMeterOn(true);
      const loop = () => {
        analyser.getFloatTimeDomainData(buf);
        let sum = 0;
        for (let i = 0; i < buf.length; i++) sum += buf[i] * buf[i];
        // RMS of speech at a sane gain is ~0.05–0.3; x4 fills the bar.
        const level = Math.min(1, Math.sqrt(sum / buf.length) * 4);
        setMicLevel(level);
        if (level > micPeak()) setMicPeak(level);
        raf = requestAnimationFrame(loop);
      };
      raf = requestAnimationFrame(loop);
    } catch (e) {
      setMicError(String(e));
    }
  }

  onCleanup(() => {
    if (meterOn()) stopMeter();
  });

  /** Mic, rung 2: record a short clip with the same MediaRecorder path the
   * plea uses, then round-trip it through the real STT route. */
  async function toggleRecord() {
    if (rec() === 'recording') {
      setRec('transcribing');
      const blob = await stopRecording();
      if (!blob || blob.size === 0) {
        setSttError('nothing captured');
        setRec('idle');
        return;
      }
      try {
        const res = await fetch('/operator/audio/stt_test', { method: 'POST', body: blob });
        if (!res.ok) {
          setSttError(await res.text());
        } else {
          const data = (await res.json()) as { transcript: string };
          setTranscript(data.transcript || '(empty — the STT service heard nothing)');
        }
      } catch (e) {
        setSttError(String(e));
      }
      setRec('idle');
      return;
    }
    setSttError('');
    setTranscript('');
    try {
      await startRecording();
      setRec('recording');
    } catch (e) {
      setSttError(String(e));
    }
  }

  return (
    <div class="panel audio-panel">
      <h2>Audio check</h2>
      <p class="muted">
        Verify the booth's ears and voice before the doors open. Server tests
        only run while the court is idle.
      </p>

      <h3>Speakers</h3>
      <p class="muted">
        Beep is browser → speakers only. Judge voice runs the full trial path:
        Kokoro synth → WebSocket → robot-voice effect → speakers.
      </p>
      <div class="btn-row">
        <button onClick={playTone}>Play beep</button>
        <button onClick={() => void runTtsTest()} disabled={ttsBusy()}>
          {ttsBusy() ? 'Synthesizing…' : 'Speak in judge voice'}
        </button>
      </div>
      <div>
        <input
          type="text"
          placeholder="custom test phrase (optional)"
          value={ttsText()}
          onInput={(e) => setTtsText(e.currentTarget.value)}
        />
      </div>
      <div class="status-line">
        <Show when={ttsStatus()}>
          <span class="status">{ttsStatus()}</span>
        </Show>
        <Show when={ttsError()}>
          <span class="err">{ttsError()}</span>
        </Show>
      </div>

      <h3>Microphone</h3>
      <p class="muted">
        The level meter proves capture; record &amp; transcribe proves the whole
        plea path (mic → upload → STT).
      </p>
      <div class="btn-row">
        <button onClick={() => void toggleMeter()}>
          {meterOn() ? 'Stop level meter' : 'Start level meter'}
        </button>
        <button onClick={() => void toggleRecord()} disabled={rec() === 'transcribing'}>
          {rec() === 'idle' && 'Record & transcribe'}
          {rec() === 'recording' && 'Stop & transcribe'}
          {rec() === 'transcribing' && 'Transcribing…'}
        </button>
      </div>
      <Show when={meterOn()}>
        <div class="mic-meter">
          <div class="mic-meter-fill" style={{ width: `${Math.round(micLevel() * 100)}%` }} />
          <div class="mic-meter-peak" style={{ left: `${Math.round(micPeak() * 100)}%` }} />
        </div>
        <p class="muted small">
          speak normally — the bar should move well past the first quarter
        </p>
      </Show>
      <Show when={rec() === 'recording'}>
        <p>
          <span class="device-badge up">
            <span class="dot" /> recording — say a test phrase, then stop
          </span>
        </p>
      </Show>
      <Show when={transcript()}>
        <p>
          heard: <strong>{transcript()}</strong>
        </p>
      </Show>
      <div class="status-line">
        <Show when={micError()}>
          <span class="err">{micError()}</span>
        </Show>
        <Show when={sttError()}>
          <span class="err">{sttError()}</span>
        </Show>
      </div>
    </div>
  );
}
