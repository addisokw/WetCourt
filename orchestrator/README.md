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

# Run
cargo run -- --config config.dev.toml
```

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

## Production (on the Spark)

The `orchestrator` service is wired into `../dgx-ai-stack/docker-compose.yml`. From the Mac:

```sh
cd dgx-ai-stack
./ai-stack    # builds and starts everything including orchestrator
```

Phase 1 still runs `hardware.driver = "mock"` in production — the real serial driver lands in Phase 4.
