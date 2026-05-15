# Wet Court orchestrator

Phase 1 skeleton: Rust state machine + axum WS server + SolidJS debug UI. Inference and hardware are mocked; no Spark or microcontroller required.

See `../wet-court-architecture.md` for the full design.

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
`http://10.10.1.221:4000`. Set `mode = "mock"` (or pass
`BOOTH__INFERENCE__MODE=mock`) to run fully offline.

Then in another shell:

```powershell
Start-Process "http://localhost:8080"
# Click "Start" or press Space — watch the banner cycle through every state.

# Or drive it without a browser:
Invoke-WebRequest -Method POST http://localhost:8080/operator/start
Invoke-WebRequest -Method POST http://localhost:8080/operator/estop
```

Frontend HMR while hacking the UI:

```powershell
cd frontend
npm run dev   # http://localhost:5173, /ws + /operator proxied to :8080
```

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
— it dials this address and speaks the §5.2 line protocol. The MCU's BOOT
button maps to `Event::OperatorStart`, so pressing it kicks off a trial
without using the browser's Start button.

## Production (on the Spark)

The `orchestrator` service is wired into `../dgx-ai-stack/docker-compose.yml`. From the Mac:

```sh
cd dgx-ai-stack
./ai-stack    # builds and starts everything including orchestrator
```

Phase 1 still runs `hardware.driver = "mock"` in production — the real serial driver lands in Phase 4.
