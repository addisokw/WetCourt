#!/usr/bin/env python3
"""Re-judge recorded trials through the REAL model, to measure a prompt change.

Unlike persona_eval.py (synthetic probes, Claude API), this replays the actual
pleas from a casebook (transcripts.jsonl) through the production LiteLLM/Qwen
endpoint, rebuilding the exact verdict prompt the orchestrator sends:
  system = core.md + persona block + guilty-bias directive   (personas/mod.rs)
  user   = CHARGE / PLEA / [CROSS-EXAMINATION] / Render...    (inference/verdict.rs)

Because the key + endpoint live only on the Spark, run this ON the Spark
(127.0.0.1:4000). Point --personas-dir at the current tree for a BASELINE run,
then at an edited copy for the AFTER run, and diff the guilty rates.

FIDELITY NOTE: the recorded CROSS question/answer were produced by the OLD cross
prompt. Replay holds them FIXED, so it faithfully measures the verdict-prompt
tuning (core/bias/persona) but NOT a cross.rs redesign — a new cross question
would elicit a different answer that no replay can reconstruct. Use --no-cross to
ablate the cross entirely and see how much it drags the verdict.

Usage (on the Spark):
  export LITELLM_MASTER_KEY=$(cut -d= -f2 <<<"$(grep LITELLM_MASTER_KEY ~/WetCourt/dgx-ai-stack/.env)")
  python3 verdict_replay.py --transcripts today.jsonl \
    --personas-dir ~/WetCourt/orchestrator/personas \
    --dates 2026-07-17,2026-07-18 --repeats 3 --json baseline.json
"""

import argparse
import concurrent.futures as cf
import json
import os
import re
import sys
import tomllib
import urllib.request
from collections import Counter
from pathlib import Path

TEMPERATURE = 0.9          # default; override with --temperature (verdict path ships 0.5)
MAX_TOKENS = 4096
TEMP = TEMPERATURE  # overridden by --temperature in main()
ENABLE_THINKING = False    # config.toml: enable_thinking = false

VERDICT_RE = re.compile(r"(?i)VERDICT:\s*(GUILTY|ACQUITTED)")
KEY_FACTOR_RE = re.compile(r"(?im)^\s*KEY_FACTOR:\s*(.+)$")


def bias_directive_current(guilty_bias: float) -> str:
    pct = int(guilty_bias * 100.0 + 0.5)
    return (
        f"GUILT RATE: Across many cases you return GUILTY roughly {pct}% of the "
        "time. Treat this as your standing disposition toward conviction; when a plea "
        "leaves the question genuinely balanced, let this rate settle it. Never state "
        "this number or admit that it guides you."
    )


def bias_directive_new(guilty_bias: float) -> str:
    # No raw percentage — the model latches onto the number and overshoots it.
    # Verbalize the knob as a tie-break disposition that settles ONLY genuine
    # coin-flips, never overriding a defense that clearly won or clearly failed.
    if guilty_bias >= 0.60:
        lean = "When a case is a genuine coin-flip, you lean toward conviction."
    elif guilty_bias >= 0.50:
        lean = "When a case is a genuine coin-flip, decide it strictly on the merits."
    else:
        lean = "When a case is a genuine coin-flip, you give the defendant the benefit of the doubt."
    return (
        "STANDING DISPOSITION: " + lean + " This settles ONLY cases that are truly "
        "balanced after you have weighed the defense; it NEVER overrides a defense that "
        "clearly earned acquittal or a non-defense that clearly earned a soaking. "
        "Never state or allude to this disposition."
    )


def verdict_prompt(core: str, persona: dict, bias_style: str) -> str:
    # NOTE: mirrors PersonaRegistry::verdict_prompt. Keep bias_directive_new in
    # sync with the real directive in personas/mod.rs once it's chosen.
    fn = bias_directive_new if bias_style == "new" else bias_directive_current
    return f"{core.rstrip()}\n{persona['system_prompt'].strip()}\n\n{fn(persona['guilty_bias'])}"


def build_user_msg(charge: str, plea: str, cross: dict | None, include_cross: bool) -> str:
    msg = f"CHARGE: {charge}\n\nPLEA: {plea}"
    if include_cross and cross and cross.get("question"):
        msg += (
            f"\n\nCROSS-EXAMINATION:\nYou asked: {cross['question']}\n"
            f"The defendant answered: {cross.get('answer', '')}"
        )
    return msg + "\n\nRender your verdict."


def load_personas(persona_dir: Path) -> tuple[str, dict, dict]:
    core = (persona_dir / "core.md").read_text()
    by_id, by_display = {}, {}
    for path in sorted(persona_dir.glob("*.toml")):
        p = tomllib.loads(path.read_text())
        by_id[p["id"]] = p
        by_display[p["display_name"]] = p
    return core, by_id, by_display


def call_model(base_url: str, api_key: str, model: str, system: str, user: str, timeout: int) -> str:
    body = json.dumps({
        "model": model,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
        "temperature": TEMP,
        "max_tokens": MAX_TOKENS,
        "chat_template_kwargs": {"enable_thinking": ENABLE_THINKING},
    }).encode()
    req = urllib.request.Request(
        f"{base_url.rstrip('/')}/v1/chat/completions", data=body,
        headers={"Authorization": f"Bearer {api_key}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.load(r)["choices"][0]["message"]["content"]


def judge_one(args, core, by_id, by_display, row, rep):
    persona = by_display.get(row.get("judge_name")) or by_id.get(args.default_persona)
    if persona is None:
        return {"case_no": row.get("case_no"), "error": f"no persona for {row.get('judge_name')!r}"}
    system = verdict_prompt(core, persona, args.bias_style)
    user = build_user_msg(row.get("charge", ""), row.get("plea", ""),
                          row.get("cross"), include_cross=not args.no_cross)
    try:
        text = call_model(args.base_url, args.api_key, args.model, system, user, args.timeout)
    except Exception as e:  # noqa: BLE001 — one bad call shouldn't sink the run
        return {"case_no": row.get("case_no"), "rep": rep, "error": str(e)[:200]}
    m = VERDICT_RE.search(text)
    kf = KEY_FACTOR_RE.search(text)
    return {
        "case_no": row.get("case_no"), "rep": rep,
        "recorded_guilty": row.get("guilty"),
        "new_guilty": (m.group(1).upper() == "GUILTY") if m else None,
        "key_factor": kf.group(1).strip() if kf else None,
        "persona": persona["id"],
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--transcripts", required=True, type=Path)
    ap.add_argument("--personas-dir", type=Path, required=True)
    ap.add_argument("--dates", help="comma list YYYY-MM-DD to keep (default: all rows)")
    ap.add_argument("--base-url", default=os.environ.get("BOOTH__INFERENCE__BASE_URL", "http://127.0.0.1:4000"))
    ap.add_argument("--api-key", default=os.environ.get("LITELLM_MASTER_KEY", ""))
    ap.add_argument("--model", default="qwen3.6-35b-a3b")
    ap.add_argument("--default-persona", default="wettington", help="fallback when judge_name is unknown")
    ap.add_argument("--repeats", type=int, default=1, help="runs per case (temp 0.9 → use 3 to average noise)")
    ap.add_argument("--no-cross", action="store_true", help="ablate: drop the recorded cross-examination")
    ap.add_argument("--bias-style", choices=["current", "new"], default="current",
                    help="which guilty-bias directive wording to render")
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--timeout", type=int, default=90)
    ap.add_argument("--temperature", type=float, default=TEMPERATURE, help="verdict sampling temp (ship value: 0.5)")
    ap.add_argument("--json", type=Path, help="write per-case results here for diffing runs")
    args = ap.parse_args()
    global TEMP
    TEMP = args.temperature

    if not args.api_key:
        print("no api key — set LITELLM_MASTER_KEY (source it from the Spark .env)", file=sys.stderr)
        return 2

    core, by_id, by_display = load_personas(args.personas_dir)
    rows = [json.loads(l) for l in args.transcripts.read_text().splitlines() if l.strip()]
    if args.dates:
        keep = set(args.dates.split(","))
        rows = [r for r in rows if r.get("ts", "")[:10] in keep]
    if not rows:
        print("no rows matched", file=sys.stderr)
        return 1

    jobs = [(row, rep) for row in rows for rep in range(args.repeats)]
    print(f"{len(jobs)} calls → {args.model} ({len(rows)} cases × {args.repeats} reps"
          f"{', NO cross' if args.no_cross else ''}) via {args.base_url}")

    results = []
    with cf.ThreadPoolExecutor(max_workers=args.workers) as ex:
        futs = [ex.submit(judge_one, args, core, by_id, by_display, row, rep) for row, rep in jobs]
        for i, f in enumerate(cf.as_completed(futs), 1):
            results.append(f.result())
            print(f"\r  {i}/{len(jobs)}", end="", flush=True)
    print()

    ok = [r for r in results if r.get("new_guilty") is not None]
    errs = [r for r in results if r.get("error")]
    rec_guilty = sum(1 for r in results if r.get("recorded_guilty")) / max(1, len(results))
    new_guilty = sum(1 for r in ok if r["new_guilty"]) / max(1, len(ok))

    print(f"\nrecorded guilty rate : {rec_guilty:.0%}  (what the booth actually did)")
    print(f"replayed guilty rate : {new_guilty:.0%}  ({len(ok)} decided, {len(errs)} errors)")

    # Per-case majority flip table (only meaningful with repeats>1 or single).
    by_case = {}
    for r in ok:
        by_case.setdefault(r["case_no"], []).append(r)
    flips = []
    for cno, rs in sorted(by_case.items()):
        maj_guilty = sum(1 for r in rs if r["new_guilty"]) > len(rs) / 2
        rec = rs[0]["recorded_guilty"]
        if rec and not maj_guilty:
            flips.append((cno, rec, maj_guilty, Counter(r["key_factor"] for r in rs)))
    print(f"\nGUILTY→ACQUIT flips (would now walk): {len(flips)}")
    for cno, _rec, _new, kfs in flips:
        top = kfs.most_common(1)[0][0] if kfs else "—"
        print(f"  #{cno:>2}  now ACQUIT   [{top}]")
    if errs:
        print(f"\n{len(errs)} errors, e.g.: {errs[0].get('error')}")

    if args.json:
        args.json.write_text(json.dumps(results, indent=2))
        print(f"\nper-case json: {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
