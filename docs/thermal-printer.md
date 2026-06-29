# Thermal-printer keepsake transcript

A physical record of a trial, printed on an 80mm thermal receipt and handed to
the defendant on their way out of the booth. It doubles as in-event marketing
(footer QR + on-site-editable booth location) and a shareable social artifact
(the guilty "moment of justice" blast photo).

This doc is the entry point for continuing the work. For the booth as a whole,
start with [`architecture.md`](architecture.md).

## Status

| Milestone | What | State |
|---|---|---|
| **M1** | Report renderer — `TrialRecord` → ESC/POS (seal, transcript, verdict, QR, reserved photo slot) | ✅ done |
| **M2** | Live wiring — capture each trial, persist the casebook log + case counter, print at verdict | ✅ done |
| **M3** | Guilty "moment of justice" blast photo from the vision still | ⬜ TODO (see below) |

## ⚠️ Before pushing

The printer driver is a **private** crate (`git.ljb.lol/lbozz4/thermal-printer`)
vendored into this repo (see [Vendoring](#vendoring)). **WetCourt's GitHub
remote is public.** Pushing a branch that contains the vendored crate publishes
that private source. Confirm the repo's visibility (or that you intend to
open-source the printer lib) before any `git push`.

## Architecture

```
state machine (Runtime)                 src/printer/
─────────────────────────               ─────────────────────────────────
 trial runs … verdict reached
        │  accumulates a TrialDraft
        │  (charge, plea, cross,
        │   verdict, judge name)
        ▼  on entering ExecutingSentence
 finalize_trial() ──────────────────▶ casebook.record(&TrialRecord)   casebook.rs
        │                                  └─ append one JSON line
        └────────────────────────────▶ print_tx ──▶ service.rs
                                              └─ render() ──▶ report.rs ──▶ ESC/POS
                                                 └─ USB write (mode = "real")
```

- **`record.rs`** — `TrialRecord`, the canonical completed-trial datum (also the
  exact JSON shape persisted to the casebook). Plus the deterministic, PII-free
  **docket-alias** generator (`The Soggy Litigant`, derived from the case number)
  and `display_time()` (friendly form of the RFC 3339 `ts`).
- **`report.rs`** — `render(&TrialRecord, &ReportOpts) -> Builder`. The full
  layout: procedurally drawn court seal, masthead, docket caption, verbatim
  transcript (charge → quoted plea → optional cross-exam → full deliberation),
  the magnified GUILTY/NOT GUILTY verdict, the **reserved photo slot**
  (`moment_of_justice`, guilty only), and the QR/booth-location footer.
  `asciify()` folds LLM/STT smart punctuation to printable ASCII.
- **`casebook.rs`** — the append-only JSONL trial log (`[logging]
  transcripts_jsonl`), and the source of truth for the case counter
  (`next_case_no()` = 1 + highest `case_no` on disk, robust to a torn final line).
- **`service.rs`** — the printer task. Receives finalized records on a channel,
  renders, and (in `real` mode) writes to the USB device on a blocking thread.
- **`mod.rs`** — re-exports.

The **capture** lives in the impure shell (`state_machine/mod.rs` `Runtime`),
*not* the pure `transitions::step`. The state machine drops charge/plea/cross
once the verdict is reached, so the Runtime harvests them off the states and the
`Deliberate` command into a `TrialDraft`, and finalizes when sentence execution
begins. The well-tested `step` function and the `State` enum are untouched.

## Configuration

`[printer]` in `config.toml` (prod) / `config.dev.toml` (dev):

```toml
[printer]
mode = "real"            # "off" | "mock" (render, no USB) | "real" (send to USB)
width_dots = 576         # 576 for a standard 80mm head; 512 on some clones
qr_url = "https://wetcourt.lol"          # footer QR target — edit on-site
booth_location = "Find the Wet Court near you"  # footer line — edit on-site
```

The **casebook log is written regardless of `mode`** — it's logging, not
printing. Its path is `[logging] transcripts_jsonl`, resolved relative to the
config file (like personas/crimes/calibration). The case counter is seeded from
it at startup.

`qr_url` and `booth_location` are the two knobs meant to change on-site as the
booth moves; editing the config and restarting is enough (no rebuild).

## Developing & testing

The renderer needs no hardware or live trial — exercise it directly:

```sh
cd orchestrator

# Render both outcomes, dump ESC/POS to the temp dir, run all printer tests:
cargo test -p booth printer::

# Same, but also send the guilty receipt to a connected USB printer:
WETCOURT_PRINT_USB=1 cargo test -p booth printer::report::tests::renders_both_outcomes
```

The dumped `.escpos` files land in the OS temp dir (paths are printed by the
test). To eyeball the layout without a printer, decode the bytes to text (strip
ESC/GS sequences) — see the commit history for a throwaway decoder, or just
print it.

End-to-end (capture → casebook → print queue) is covered hermetically by
`state_machine::tests::trial_finalizes_into_casebook_and_print_queue`, which
drives a whole trial through explicit events.

Drive a real (mocked-infra) trial and watch the log populate:

```sh
cd orchestrator
BOOTH__INFERENCE__MODE=mock BOOTH__HARDWARE__DRIVER=mock \
BOOTH__CROSS_EXAMINATION__ENABLED=false \
cargo run -- --config config.dev.toml
# then: POST http://localhost:8080/operator/start
# watch ./transcripts.jsonl gain a line, and the log show "keepsake rendered"
```

## Vendoring

The printer driver crate lives at `orchestrator/crates/thermal-printer`, a
workspace member. It was brought in with `git subtree` (squashed) from the
private repo — **not** a submodule (the source repo is private and we don't want
a submodule pointer to it).

Update it from upstream with:

```sh
# `thermal-src` remote points at the local clone of the private repo
git subtree pull --prefix=orchestrator/crates/thermal-printer thermal-src main --squash
```

Re-add the remote if a fresh clone doesn't have it:

```sh
git remote add thermal-src /path/to/thermal-printer   # local clone
git fetch thermal-src
```

See the ⚠️ note above re: pushing vendored private code to the public remote.

## M3 — the "moment of justice" blast photo (next)

The guilty receipt reserves a framed slot (`report::moment_of_justice`,
currently a placeholder reticle). Fill it with the firing-still from the vision
service:

1. **Vision endpoint** — add `GET /still.jpg` to [`vision/vision.py`] returning
   the latest captured frame (`_latest_jpeg`). The capture loop already keeps it;
   this just serves it. (The README there already flags a "firing-still" as
   planned.)
2. **Capture timing** — in `Runtime`, when entering `ExecutingSentence` for a
   guilty verdict, fetch the still shortly after the FIRE command so the water is
   mid-air. A short delay / small burst-and-pick may be needed; tune against the
   real squirt. The orchestrator already reaches the vision service via
   `cfg.vision.base_url` (reverse-proxied at `/vision/*`).
3. **Into the receipt** — dither the JPEG to 1-bit with
   `thermal_printer::raster::from_image` (Atkinson or Floyd–Steinberg) at the
   printer width, and pass it on the `TrialRecord` so `moment_of_justice` rasters
   it instead of the placeholder. Add an `Option<Raster>`-shaped field (or raw
   bytes) to `TrialRecord` for this; keep it out of the casebook JSON (log a path
   or omit).

Order of operations matters: the print is dispatched at finalize, so the still
must be captured *before* `finalize_trial()` sends the record — i.e. fetch the
frame during the `ExecutingSentence` entry, attach it to the draft, then
finalize.
