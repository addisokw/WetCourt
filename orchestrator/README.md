# Wet Court orchestrator

The booth's brain: a Rust state machine driving the full trial pipeline —
charge selection (curated list), plea recording (browser mic), transcription
(STT), deliberation (streamed LLM), verdict pronunciation (pipelined TTS),
and the sentence (squirt hardware) — plus an axum HTTP/WS server and a
SolidJS frontend with operator console, judge face, and case view.

See `../docs/architecture.md` for the full design. All phases through 4
(WiFi hardware) are implemented; only the USB-serial hardware driver remains
a stub.

## Trial flow

```
Idle ─► GeneratingCharge ─► DisplayingCharge ─► AwaitingPlea ─► FlushingPlea
     ─► Transcribing ─► [Cross-examination] ─► Deliberating ─► PronouncingVerdict
     ─► ExecutingSentence ─► Idle
```

When cross-examination is enabled (operator-toggleable; see below), the first
plea routes through a one-question loop — `CrossGeneratingQuestion ─►
CrossSpeaking ─► CrossAwaitingAnswer ─► CrossTranscribing` — and the answer is
folded into the deliberation prompt. It's skipped when the defendant offered no
plea, and any cross-exam timeout falls straight through to the verdict.

Any failure drops into `Error` (auto-recovers) with canned fallback charges
and verdicts, so the booth never stalls in front of a visitor. An e-stop is
honored from every state.

## Deployment topologies

The orchestrator's location is a config knob, not an architectural
assumption — same binary, same code path, different `inference.base_url`.
Two shapes are supported, and the checked-in config files are the starting
points for each:

**A. Everything on the Spark** (`config.toml`) — the orchestrator runs as a
container in `../dgx-ai-stack/docker-compose.yml` next to the inference
stack. `./ai-stack` brings it all up together.

**B. Inference on the Spark, orchestrator anywhere** (`config.dev.toml`) —
`cargo run` on a laptop or booth PC that reaches LiteLLM over the network.
This is the everyday dev loop, but it's equally valid for production: the
MCU dials the orchestrator over WiFi, so real hardware works from any host
the MCU can reach. (Today this shape needs *no* compose change for hardware,
unlike A — see the `:8090` gap note under Production.)

What actually differs:

| Knob | A: co-located (`config.toml`) | B: remote (`config.dev.toml`) |
|---|---|---|
| `inference.base_url` | `http://litellm:4000` (docker network) | Spark's LAN/Tailscale address, e.g. `http://100.86.115.53:4000` |
| API key | injected by compose from the Spark's `.env` | auto-loaded from `.env` (repo root or `dgx-ai-stack/.env`) |
| `hardware.driver` | `tcp` — MCU dials the Spark's `:8090` | `mock` by default; set `tcp` and the MCU dials this machine's `:8090` |
| Personas + crime list | bind-mounted from the Spark's checkout | read/written in the local checkout |
| Logs / transcripts | `booth-logs` volume (`/var/log/booth/`) | `./booth.log`, `./transcripts.jsonl` |
| Kiosk / monitor URLs | `http://<spark>:8080` | `http://<this machine>:8080` |

Prefer overriding single knobs with `BOOTH__…` env vars (e.g.
`BOOTH__HARDWARE__DRIVER=tcp`) over editing the files. If you run shape B
against a Spark whose own orchestrator container is up, stop the Spark's
copy so there aren't two consoles:
`cd ../dgx-ai-stack && ./ai-stack stop orchestrator`.

## Dev quick start — shape B (no MCU, no Docker)

```powershell
# One-time
cd frontend
npm install
npm run build
cd ..

# Run (loads LITELLM_MASTER_KEY → BOOTH__INFERENCE__API_KEY from a .env in
# any ancestor dir, falling back to ../dgx-ai-stack/.env — so the one file
# created from dgx-ai-stack/.env.example covers this too)
cargo run -- --config config.dev.toml
```

`config.dev.toml` defaults to `inference.mode = "real"` against the Spark at
`http://10.10.1.221:4000` with `hardware.driver = "mock"`. Set
`mode = "mock"` (or pass `BOOTH__INFERENCE__MODE=mock`) to run fully offline
— mock inference returns canned charges/verdicts with simulated latency.

Any config key can be overridden via env vars prefixed `BOOTH__` with `__`
as the section separator, e.g. `BOOTH__TRIAL__PLEA_WINDOW_SECS=30`.

Frontend HMR while hacking the UI:

```powershell
cd frontend
npm run dev   # http://localhost:5173, /ws + /operator proxied to :8080
```

(The release binary embeds the built `frontend/dist` via rust-embed, so
`npm run build` must run before `cargo run` picks up UI changes.)

## Views

Everything is served on `:8080` (`display.listen_addr`):

| URL | View | Audience |
|---|---|---|
| `/` | Operator console: state banner, Start/Plea/E-Stop, judge-face + case-view preview panes, event log, persona panel | Operator |
| `/face` | Standalone animated ASCII judge face (moods track listening/thinking/speaking/verdict) | Visitor-facing monitor |
| `/case` | Standalone case view: charge, plea countdown, transcript, verdict | Visitor-facing monitor |

Keyboard on the console: **Space** starts a trial, **P** starts/stops plea
recording (browser asks for mic permission on first use). Plea audio is
captured with MediaRecorder and uploaded over the WS as binary.

The console uses the single-client `/ws` socket (read+write); `/face` and
`/case` use the multi-client read-only `/ws/view`, so you can mirror them on
as many monitors as you like. During the verdict reveal the frontend runs a
"deliberation theater" beat — an ambient synth pad and dimmed visuals over a
held silence before the guilty/not-guilty word lands.

## Operator HTTP API

```
POST /operator/start                    kick off a trial (same as Space / MCU button)
POST /operator/estop                    emergency stop from any state
GET  /operator/personas                 list personas + active id
GET  /operator/voices                   Kokoro voice catalogue
GET  /operator/persona                  fetch the active persona
POST /operator/persona                  create a persona
PUT  /operator/persona/{id}             edit a persona
POST /operator/persona/{id}/select      make it the active judge
POST /operator/persona/{id}/save        persist to personas/{id}.toml
POST /operator/persona/{id}/test        dry-run a charge+plea, returns deliberation/verdict
GET  /operator/crimes                   full crime list + categories + draw filter + queue
POST /operator/crimes                   add a crime {category, charge} (persists)
PUT  /operator/crimes/{id}              edit a crime (text/category/enabled, persists)
DELETE /operator/crimes/{id}            remove a crime (persists)
POST /operator/crimes/filter            {category} restricts random draws; {category: null} clears
POST /operator/crimes/queue             {charge} queue a manual charge for the next trial
DELETE /operator/crimes/queue/{index}   drop a queued charge
GET  /operator/cross_exam               {enabled} current cross-examination toggle
POST /operator/cross_exam               {enabled} turn cross-examination on/off (live)
GET  /health                            liveness probe
```

## Charges (curated crime list)

Charges are drawn from `crimes/wet_court_crimes.json` — a flat list of
`{id, category, charge, enabled}` entries — instead of being LLM-generated,
so operators control exactly what the booth can accuse people of. Selection
order at trial start:

1. **Operator queue** — charges typed into the console's Crimes panel run
   next, in order (the "manual charge input" idea).
2. **Random draw** from enabled crimes, honoring the optional **category
   filter** (set "draw only from" in the panel — this is how creator-specific
   charge sets work: tag them with a category and filter to it), and avoiding
   the last `no_repeat_window` draws.
3. **Canned fallback** if the list is empty or fully filtered out.

The Crimes panel on the operator console is the curation tool: add, edit,
delete, enable/disable per crime, all persisted straight back to the JSON
file. `[crimes] source = "llm"` in config restores the old on-the-fly
generation (the queue still takes precedence).

## Judge personas

Personas live in `personas/*.toml` next to the config file — each defines
`display_name`, `system_prompt`, `guilty_bias`, `tts_voice`, and optional
`tts_speed`. Persona prompts are kept **bias-free**: they describe character
and *what kinds of pleas* sway the judge, but never a conviction rate. The
`guilty_bias` slider is injected into the prompt at trial start as a target
guilt rate, so it is the single knob that tunes how often a judge convicts.
The squirt gun is binary — every guilty verdict fires one fixed duration
(`[squirt] duration_ms`); there is no per-verdict intensity.

Six ship in-repo: **Justice Wettington** (default — petty, theatrical,
`bm_george`), **Judge Bom** (curt but fair, `am_onyx`), **Judge Sunny Vale**
(relentlessly cheerful, `af_heart`), **Judge Magnus Thorne** (thunderous and
biblical, `am_fenrir`), **Judge Remy Calhoun** (cold actuarial bureaucrat,
`bm_daniel`), and **Dame Beatrix Plume** (witheringly polite, `bf_emma`). The
persona panel on the operator console can create, edit, voice-swap, test, and
hot-select personas mid-session; the active persona is snapshotted at trial
start.

## Failure injection

Edit `config.dev.toml`:

```toml
[mock_hw]
fail_rate = 0.5                 # half of hardware acks become errors
simulate_estop_after_secs = 8   # synthetic ESTOP 8 s after orchestrator startup
```

## Real hardware over WiFi (M5Stack NanoC6)

Set `[hardware] driver = "tcp"` and `bind_addr = "0.0.0.0:8090"` in the
config you're using. Then flash and configure the firmware in `../firmware/`
— it dials this address and speaks the §5.2 line protocol (`FIRE`, `GAVEL`,
`LIGHTS`, `PANEL`, `PING`). The MCU's BOOT button maps to
`Event::OperatorStart`, so pressing it kicks off a trial without the
browser. `driver = "serial"` (USB) is declared in config but unimplemented.

## Production on the Spark — shape A

The `orchestrator` service is wired into `../dgx-ai-stack/docker-compose.yml`
(multi-stage Dockerfile: frontend build → Rust build → slim runtime). From
the Mac:

```sh
cd dgx-ai-stack
./ai-stack    # builds and starts everything including orchestrator
```

Production `config.toml` runs `inference.mode = "real"` against
`http://litellm:4000` and `hardware.driver = "tcp"` listening on `:8090` for
the MCU. **Gap:** the compose file currently publishes only `8080`, so the
MCU can't reach the containerized `:8090` listener — add a
`"0.0.0.0:8090:8090"` port mapping before pairing real hardware in
production (laptop-hosted runs are unaffected). Logs and trial transcripts
(JSONL) land in the `booth-logs` volume at `/var/log/booth/`.
