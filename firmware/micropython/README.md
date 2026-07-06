# firmware/micropython — shared MicroPython support code

**Not a board.** This is the one shared artifact among the MicroPython NanoC6
boards (`judge-neck`, `turret`, `squirt`, `gavel`): each board's `deploy.sh`
copies `wetline.py` from here onto its device, so the fleet's protocol client
has a single source of truth instead of four drifting copies.

`wetline.py` is the role-agnostic half of a device firmware: WiFi bring-up,
dialing the orchestrator, the `HELLO` handshake, the non-blocking line loop
with one-ack-per-command (see [`../../protocol/README.md`](../../protocol/README.md)),
reconnect with backoff, and the NanoC6's RGB LED as a link-status light
(**red** = WiFi down · **amber** = dialing · **green** = connected). Each
board's `main.py` supplies only its role name and verb handlers.

Runtime: MicroPython **v1.28.0 `ESP32_GENERIC_C6`** (flash at offset `0x0` —
the C6 is RISC-V). Flashing instructions live in each board's README.
