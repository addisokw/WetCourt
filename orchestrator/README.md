# Wet Court orchestrator

The booth's brain: a Rust state machine driving the full trial pipeline —
charge generation (LLM), plea recording (browser mic), transcription (STT),
deliberation (streamed LLM), verdict pronunciation (pipelined TTS), and the
sentence (squirt hardware) — plus an axum HTTP/WS server and a SolidJS
frontend with operator console, judge face, and case view.

See `../docs/architecture.md` for the full design. All phases through 4
(WiFi hardware) are implemented; only the USB-serial hardware driver remains
a stub.

## Trial flow

```
Idle ─► GeneratingCharge ─► DisplayingCharge ─► AwaitingPlea ─► FlushingPlea
     ─► Transcribing ─► Deliberating ─► PronouncingVerdict ─► ExecutingSentence ─► Idle
```

Any failure drops into `Error` (auto-recovers) with canned fallback charges
and verdicts, so the booth never stalls in front of a visitor. An e-stop is
honored from every state.

## Dev (no MCU, no Docker)

```powershell
# One-time
cd frontend
npm install
npm run build
cd ..

# Run (auto-loads ../.env for LITELLM_MASTER_KEY → BOOTH__INFERENCE__API_KEY)
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
| `/case` | Standalone case view: charge, plea countdown, transcript, verdict + intensity | Visitor-facing monitor |

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
POST /operator/persona/{id}/test        dry-run a charge+plea, returns deliberation/verdict/intensity
GET  /health                            liveness probe
```

## Judge personas

Personas live in `personas/*.toml` next to the config file — each defines
`display_name`, `system_prompt`, `guilty_bias`, `tts_voice`, and optional
`tts_speed`. Two ship in-repo: **Justice Wettington** (default — petty,
theatrical, guilty_bias 0.7, voice `bm_george`) and **Judge Bom** (curt but
fair, 0.5, `am_onyx`). The persona panel on the operator console can create,
edit, voice-swap, test, and hot-select personas mid-session; the active
persona is snapshotted at trial start.

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

## Production (on the Spark)

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
