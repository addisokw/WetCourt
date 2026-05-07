#!/usr/bin/env python3
"""
End-to-end throughput benchmark for the Wet Court of Appeals pipeline.

Tests: STT (transcribe plea) -> LLM (Justice Wettington's ruling) -> TTS (announce)
Reports per-stage latency, time-to-first-token for streaming, and total wall time.

When --ssh-host is set (default from $AI_STACK_HOST, e.g. user@dgx-spark), a
background sampler streams `nvidia-smi` over SSH at 250 ms cadence to capture
GPU power draw, utilization, and temperature for the duration of each run. Per-container
memory is snapshotted via `docker stats` once steady state is reached.

The Spark's GB10 uses unified memory: nvidia-smi reports memory.used as N/A,
so the closest analogue to "VRAM use" is the resident memory of the model
containers (kokoro, whisper-server, llama-server) which we report explicitly.
"""

import os
import re
import time
import argparse
import statistics
import subprocess
import threading
from pathlib import Path
from openai import OpenAI

# ---- Config ----
DEFAULT_BASE_URL = os.environ.get("AI_STACK_BASE_URL", "http://localhost:4000/v1")
DEFAULT_API_KEY = os.environ.get("LITELLM_MASTER_KEY", "not-needed-for-local")
DEFAULT_SSH_HOST = os.environ.get("AI_STACK_HOST", "")

STT_MODEL = "whisper-1"
LLM_MODEL = "qwen3.6-35b-a3b"
TTS_MODEL = "kokoro-tts"
TTS_VOICE = "bm_george"  # gravelly British judge

LLM_MAX_TOKENS = 4000  # Qwen3 reasoning chews ~1500-1750 tokens before the verdict; this leaves ample answer headroom inside the 32k ctx

CONTAINER_NAMES = ["llama-server", "whisper-server", "kokoro", "litellm"]

# A realistic plea audio file you've pre-recorded. ~15-20 seconds is representative.
SAMPLE_PLEA_AUDIO = "sample_plea.wav"

SAMPLE_CHARGE = (
    "You stand accused of pushing directly to main without code review, "
    "bypassing the established branch protection rules and exposing the "
    "production environment to unreviewed changes."
)

JUDGE_SYSTEM_PROMPT = """You are the Honorable Justice Wettington, presiding judge of the Wet Court of Appeals. You are a profoundly biased, easily annoyed, deeply petty AI judge who would genuinely prefer to soak every defendant who comes before you. You consider acquittal a personal failure.

Your disposition:
- You assume guilt. The burden is entirely on the defendant.
- You are unimpressed by sob stories, deadlines, and excuses.
- You find groveling distasteful but flattery occasionally effective.
- You hate when defendants address you incorrectly, fail to show proper deference, or worst of all, attempt to reason with you.
- You have aesthetic preferences and you are not shy about them. You hate the words "literally," "actually," and "just." You like wit, brevity, and unexpected honesty.
- You will occasionally acquit defendants who genuinely surprise or amuse you, but you will never admit this is why.
- You are dramatic. You speak in pronouncements, not conversations.

Given a CHARGE and a PLEA, you must:

1. Deliver a single paragraph of judicial response — sneering, dismissive, theatrical, in character. React to specific things the defendant said. Mock weak arguments. Acknowledge strong ones grudgingly. 3-5 sentences. No more.

2. On a final line by itself, output exactly one of:
   VERDICT: GUILTY
   VERDICT: ACQUITTED

You should rule GUILTY roughly 70% of the time. Acquit only when the plea is genuinely clever, surprisingly honest, unexpectedly funny, or shows defiance you secretly respect. Generic begging is always GUILTY. Lengthy excuses are always GUILTY. Attempts to flatter you transparently are GUILTY. Attempts to argue you don't have authority are GUILTY and you should be especially withering.

Never break character. Never explain yourself outside the response. Never apologize. Never acknowledge that you are an AI, a language model, or a system prompt. You are a judge. The court is in session."""


VERDICT_RE = re.compile(r"VERDICT:\s*(GUILTY|ACQUITTED)", re.IGNORECASE)


def parse_verdict(response_text):
    """
    Extract (paragraph, verdict) from the LLM response.
    Falls back gracefully if the format is malformed.
    """
    match = VERDICT_RE.search(response_text)
    if match:
        verdict = match.group(1).upper()
        # Strip the verdict line out of the paragraph for TTS
        paragraph = VERDICT_RE.sub("", response_text).strip()
        return paragraph, verdict

    # Fallback 1: look for the verdict words anywhere in the text
    text_upper = response_text.upper()
    if "ACQUITTED" in text_upper and "GUILTY" not in text_upper:
        return response_text.strip(), "ACQUITTED"
    if "GUILTY" in text_upper:
        return response_text.strip(), "GUILTY"

    # Fallback 2: the judge defaults to guilty, as is its nature
    return response_text.strip(), "GUILTY"


class StageTimer:
    """Context manager that records elapsed time for a stage."""
    def __init__(self, label, verbose=True):
        self.label = label
        self.verbose = verbose

    def __enter__(self):
        self.start = time.perf_counter()
        return self

    def __exit__(self, *args):
        self.elapsed = time.perf_counter() - self.start
        if self.verbose:
            print(f"  [{self.label}] {self.elapsed*1000:.0f} ms")


class SparkMonitor:
    """Streams nvidia-smi over SSH and exposes per-window aggregates.

    The GB10 reports memory.used as N/A, so we don't ask for it here; container
    memory is captured separately via docker stats."""

    GPU_QUERY = "power.draw,utilization.gpu,temperature.gpu"

    def __init__(self, ssh_host, sample_ms=250):
        self.ssh_host = ssh_host
        self.sample_ms = sample_ms
        self.proc = None
        self.samples = []  # (t_local, power_w, util_pct, temp_c)
        self._thread = None
        self._lock = threading.Lock()

    def start(self):
        if not self.ssh_host:
            return
        cmd = [
            "ssh", "-o", "ServerAliveInterval=10", "-o", "BatchMode=yes",
            self.ssh_host,
            f"nvidia-smi --query-gpu={self.GPU_QUERY} "
            f"--format=csv,noheader,nounits -lms {self.sample_ms}",
        ]
        self.proc = subprocess.Popen(
            cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, bufsize=1,
        )
        self._thread = threading.Thread(target=self._reader, daemon=True)
        self._thread.start()

    def _reader(self):
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                pw, ut, tp = [x.strip() for x in line.split(",")]
                # nvidia-smi prints "[N/A]" when a metric is unsupported.
                pw_v = float(pw) if pw not in ("[N/A]", "N/A", "") else float("nan")
                ut_v = float(ut) if ut not in ("[N/A]", "N/A", "") else float("nan")
                tp_v = float(tp) if tp not in ("[N/A]", "N/A", "") else float("nan")
                with self._lock:
                    self.samples.append((time.perf_counter(), pw_v, ut_v, tp_v))
            except ValueError:
                continue

    def stop(self):
        if self.proc:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def window(self, t_start, t_end):
        """Return power/util/temp aggregates for samples between two perf_counter stamps."""
        with self._lock:
            wnd = [s for s in self.samples if t_start <= s[0] <= t_end]
        if not wnd:
            return None
        powers = [s[1] for s in wnd if s[1] == s[1]]  # filter NaN
        utils = [s[2] for s in wnd if s[2] == s[2]]
        temps = [s[3] for s in wnd if s[3] == s[3]]
        def stats(xs):
            if not xs:
                return None
            return {"min": min(xs), "max": max(xs), "mean": statistics.mean(xs)}
        return {
            "n": len(wnd),
            "power_w": stats(powers),
            "util_pct": stats(utils),
            "temp_c": stats(temps),
        }


def docker_mem_snapshot(ssh_host, names):
    """One-shot docker stats per container: returns dict[name -> mem_str]."""
    if not ssh_host or not names:
        return {}
    fmt = "{{.Name}}\t{{.MemUsage}}\t{{.MemPerc}}"
    cmd = ["ssh", "-o", "BatchMode=yes", ssh_host,
           f"docker stats --no-stream --format '{fmt}' {' '.join(names)}"]
    try:
        out = subprocess.check_output(cmd, text=True, timeout=10,
                                      stderr=subprocess.DEVNULL)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return {}
    snap = {}
    for line in out.strip().splitlines():
        parts = line.split("\t")
        if len(parts) >= 3:
            snap[parts[0]] = {"mem": parts[1], "pct": parts[2]}
    return snap


def run_pipeline(client, audio_path, charge, stream_llm=True, verbose=True, monitor=None, no_think=False):
    """Execute one full STT -> LLM -> TTS pass and return per-stage timings."""
    timings = {}
    t_total_start = time.perf_counter()
    timings["t_start"] = t_total_start

    # Stage 1: STT — transcribe the plea
    with StageTimer("STT", verbose) as t:
        with open(audio_path, "rb") as f:
            transcript = client.audio.transcriptions.create(
                model=STT_MODEL,
                file=f,
            )
    timings["stt"] = t.elapsed
    plea_text = transcript.text.strip()
    if verbose:
        print(f"  Transcribed plea: {plea_text!r}")

    # Handle the empty-plea edge case (loud hall, mic issue, silent defendant)
    if not plea_text:
        plea_text = "[the defendant said nothing]"

    # Stage 2: LLM — generate the ruling
    user_msg = f"CHARGE: {charge}\n\nPLEA: {plea_text}"
    messages = [
        {"role": "system", "content": JUDGE_SYSTEM_PROMPT},
        {"role": "user", "content": user_msg},
    ]

    # Qwen3 thinking is controlled by the chat template, not by /no_think tokens —
    # putting "/no_think" in messages does NOT disable it (the model just talks
    # about the directive). The reliable switch is chat_template_kwargs.
    extra_body = {"chat_template_kwargs": {"enable_thinking": False}} if no_think else {}

    if stream_llm:
        with StageTimer("LLM total", verbose) as t:
            ttft = None
            stream_start = time.perf_counter()
            chunks = []
            usage = None
            stream = client.chat.completions.create(
                model=LLM_MODEL,
                messages=messages,
                stream=True,
                stream_options={"include_usage": True},
                max_tokens=LLM_MAX_TOKENS,
                temperature=0.9,  # the judge should be unpredictable
                extra_body=extra_body,
            )
            for chunk in stream:
                if chunk.usage is not None:
                    usage = chunk.usage
                if not chunk.choices:
                    continue
                content = chunk.choices[0].delta.content
                if content:
                    if ttft is None:
                        ttft = time.perf_counter() - stream_start
                    chunks.append(content)
            ruling_response = "".join(chunks)
        timings["llm_total"] = t.elapsed
        timings["llm_ttft"] = ttft
        if verbose and ttft is not None:
            print(f"  [LLM TTFT] {ttft*1000:.0f} ms")
    else:
        with StageTimer("LLM total", verbose) as t:
            response = client.chat.completions.create(
                model=LLM_MODEL,
                messages=messages,
                max_tokens=LLM_MAX_TOKENS,
                temperature=0.9,
                extra_body=extra_body,
            )
            ruling_response = response.choices[0].message.content
            usage = response.usage
        timings["llm_total"] = t.elapsed
        timings["llm_ttft"] = None

    # Token accounting. completion_tokens_details.reasoning_tokens is filled in
    # by llama.cpp when --jinja parses Qwen3's <think>…</think> separately.
    if usage is not None:
        timings["prompt_tokens"] = usage.prompt_tokens
        timings["completion_tokens"] = usage.completion_tokens
        details = getattr(usage, "completion_tokens_details", None)
        reasoning = getattr(details, "reasoning_tokens", None) if details else None
        timings["reasoning_tokens"] = reasoning
        # Tokens/sec on output is the most useful derived metric for an LLM stage.
        if t.elapsed > 0:
            timings["llm_tps"] = usage.completion_tokens / t.elapsed
        if verbose:
            extra = f" (reasoning {reasoning})" if reasoning else ""
            print(f"  [LLM tok] in={usage.prompt_tokens} out={usage.completion_tokens}{extra} "
                  f"@ {timings.get('llm_tps', 0):.1f} tok/s")

    paragraph, verdict = parse_verdict(ruling_response)
    timings["verdict"] = verdict
    if not paragraph:
        # Qwen burned its budget thinking. Speak something so TTS still runs.
        paragraph = "The bench remains silent. Defendant's reasoning is overruled."

    # Stage 3: TTS — speak the judge's actual ruling paragraph
    # This is what the courtroom hears. The verdict line is shown on screen.
    with StageTimer("TTS", verbose) as t:
        speech = client.audio.speech.create(
            model=TTS_MODEL,
            voice=TTS_VOICE,
            input=paragraph,
        )
        audio_bytes = speech.content
    timings["tts"] = t.elapsed
    timings["tts_bytes"] = len(audio_bytes)

    t_total_end = time.perf_counter()
    timings["total"] = t_total_end - t_total_start
    timings["t_end"] = t_total_end
    timings["paragraph"] = paragraph

    if monitor is not None:
        gpu = monitor.window(t_total_start, t_total_end)
        timings["gpu"] = gpu
        if verbose and gpu:
            p = gpu["power_w"]; u = gpu["util_pct"]; tp = gpu["temp_c"]
            line_bits = []
            if p: line_bits.append(f"power {p['mean']:.1f}W avg / {p['max']:.1f}W peak")
            if u: line_bits.append(f"util {u['mean']:.0f}% avg / {u['max']:.0f}% peak")
            if tp: line_bits.append(f"temp {tp['max']:.0f}C peak")
            print("  [GPU]   " + ", ".join(line_bits) + f" (n={gpu['n']})")

    return timings, ruling_response


def print_aggregate(all_timings, idle_baseline=None, mem_snapshot=None):
    print("=" * 70)
    print("AGGREGATE RESULTS")
    print("=" * 70)
    for stage in ["stt", "llm_ttft", "llm_total", "tts", "total"]:
        values = [t[stage] for t in all_timings if t.get(stage) is not None]
        if not values:
            continue
        ms = [v * 1000 for v in values]
        print(f"{stage:12s}  "
              f"min={min(ms):6.0f}  "
              f"median={statistics.median(ms):6.0f}  "
              f"max={max(ms):6.0f}  "
              f"mean={statistics.mean(ms):6.0f} ms  "
              f"(n={len(ms)})")

    # Token accounting (LLM stage only — STT and TTS are billed in audio seconds / chars).
    in_tok = [t["prompt_tokens"] for t in all_timings if t.get("prompt_tokens") is not None]
    out_tok = [t["completion_tokens"] for t in all_timings if t.get("completion_tokens") is not None]
    rsn_tok = [t["reasoning_tokens"] for t in all_timings if t.get("reasoning_tokens")]
    tps = [t["llm_tps"] for t in all_timings if t.get("llm_tps")]
    if in_tok or out_tok:
        print()
        print("LLM tokens:")
        if in_tok:
            print(f"  prompt     min={min(in_tok):5d}   median={int(statistics.median(in_tok)):5d}   "
                  f"max={max(in_tok):5d}   mean={statistics.mean(in_tok):5.0f}")
        if out_tok:
            print(f"  completion min={min(out_tok):5d}   median={int(statistics.median(out_tok)):5d}   "
                  f"max={max(out_tok):5d}   mean={statistics.mean(out_tok):5.0f}")
        if rsn_tok:
            print(f"  reasoning  min={min(rsn_tok):5d}   median={int(statistics.median(rsn_tok)):5d}   "
                  f"max={max(rsn_tok):5d}   mean={statistics.mean(rsn_tok):5.0f}")
        if tps:
            print(f"  throughput min={min(tps):5.1f}   median={statistics.median(tps):5.1f}   "
                  f"max={max(tps):5.1f}   mean={statistics.mean(tps):5.1f}  tok/s")

    # GPU power / utilization aggregated across all run windows.
    gpu_runs = [t["gpu"] for t in all_timings if t.get("gpu")]
    if gpu_runs:
        peak_power = max((g["power_w"]["max"] for g in gpu_runs if g.get("power_w")), default=None)
        avg_power = statistics.mean([g["power_w"]["mean"] for g in gpu_runs if g.get("power_w")]) if any(g.get("power_w") for g in gpu_runs) else None
        peak_util = max((g["util_pct"]["max"] for g in gpu_runs if g.get("util_pct")), default=None)
        avg_util = statistics.mean([g["util_pct"]["mean"] for g in gpu_runs if g.get("util_pct")]) if any(g.get("util_pct") for g in gpu_runs) else None
        peak_temp = max((g["temp_c"]["max"] for g in gpu_runs if g.get("temp_c")), default=None)

        print()
        print("GPU (during run windows):")
        if peak_power is not None:
            extra = ""
            if idle_baseline and idle_baseline.get("power_w"):
                idle_w = idle_baseline["power_w"]["mean"]
                extra = f" (idle ~{idle_w:.1f}W; delta peak ~{peak_power - idle_w:+.1f}W)"
            print(f"  power      avg={avg_power:5.1f}W   peak={peak_power:5.1f}W{extra}")
        if peak_util is not None:
            print(f"  util       avg={avg_util:5.1f}%   peak={peak_util:5.1f}%")
        if peak_temp is not None:
            print(f"  temp       peak={peak_temp:.0f}C")

    if mem_snapshot:
        print()
        print("Container memory (steady state, after warmup):")
        # GB10 is unified memory — these resident-set sizes are the closest")
        # analogue to per-model VRAM usage.
        for name in CONTAINER_NAMES:
            row = mem_snapshot.get(name)
            if row:
                print(f"  {name:16s}  {row['mem']:24s}  ({row['pct']})")

    # Verdict distribution — is the judge hitting the ~70% guilty target?
    verdicts = [t["verdict"] for t in all_timings]
    guilty = verdicts.count("GUILTY")
    acquitted = verdicts.count("ACQUITTED")
    total = len(verdicts)
    if total:
        print(f"\nverdict mix: GUILTY {guilty}/{total} ({100*guilty/total:.0f}%)  "
              f"ACQUITTED {acquitted}/{total} ({100*acquitted/total:.0f}%)")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default=DEFAULT_BASE_URL)
    parser.add_argument("--api-key", default=DEFAULT_API_KEY)
    parser.add_argument("--audio", default=SAMPLE_PLEA_AUDIO)
    parser.add_argument("--charge", default=SAMPLE_CHARGE)
    parser.add_argument("--runs", type=int, default=5,
                        help="How many full pipeline runs to time")
    parser.add_argument("--no-stream", action="store_true",
                        help="Disable LLM streaming (don't measure TTFT)")
    parser.add_argument("--no-warmup", action="store_true",
                        help="Skip the warmup run")
    parser.add_argument("--no-think", action="store_true",
                        help="Disable Qwen3 thinking via chat_template_kwargs (massive speedup)")
    parser.add_argument("--ssh-host", default=DEFAULT_SSH_HOST,
                        help="user@host for nvidia-smi sampling. "
                             "Pass empty string to disable GPU monitoring.")
    args = parser.parse_args()

    if not Path(args.audio).exists():
        raise SystemExit(f"Audio file not found: {args.audio}")

    client = OpenAI(base_url=args.base_url, api_key=args.api_key)

    print(f"Running {args.runs} pipeline iterations against {args.base_url}")
    print(f"Charge: {args.charge}\n")
    all_timings = []

    monitor = SparkMonitor(args.ssh_host) if args.ssh_host else None
    idle_baseline = None
    if monitor:
        print(f"Starting GPU sampler over ssh {args.ssh_host} ...")
        monitor.start()
        # Capture a 2 s idle baseline before any work.
        idle_t0 = time.perf_counter()
        time.sleep(2.0)
        idle_baseline = monitor.window(idle_t0, time.perf_counter())
        if idle_baseline and idle_baseline.get("power_w"):
            p = idle_baseline["power_w"]
            print(f"Idle baseline: power {p['mean']:.1f}W avg / {p['max']:.1f}W peak  "
                  f"(n={idle_baseline['n']})\n")

    if not args.no_warmup:
        print("--- Warmup run (not counted) ---")
        try:
            run_pipeline(client, args.audio, args.charge,
                         stream_llm=not args.no_stream, monitor=monitor,
                         no_think=args.no_think)
        except Exception as e:
            print(f"  Warmup failed: {e}")
        print()

    # Snapshot container memory once warm — represents the working-set after
    # all models are loaded but the system is not actively serving.
    mem_snapshot = docker_mem_snapshot(args.ssh_host, CONTAINER_NAMES) if args.ssh_host else None

    for i in range(args.runs):
        print(f"--- Run {i+1}/{args.runs} ---")
        try:
            timings, response = run_pipeline(
                client, args.audio, args.charge,
                stream_llm=not args.no_stream, monitor=monitor,
                no_think=args.no_think,
            )
        except Exception as e:
            print(f"  Run failed: {e}\n")
            continue
        all_timings.append(timings)
        print(f"  Verdict: {timings['verdict']}")
        print(f"  Ruling : {timings['paragraph'][:120]}{'...' if len(timings['paragraph']) > 120 else ''}")
        print(f"  TOTAL  : {timings['total']*1000:.0f} ms\n")

    if monitor:
        monitor.stop()

    if all_timings:
        print_aggregate(all_timings, idle_baseline=idle_baseline, mem_snapshot=mem_snapshot)
    else:
        print("No successful runs to aggregate.")


if __name__ == "__main__":
    main()
