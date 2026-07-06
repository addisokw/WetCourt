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

## OTA updates (WiFi, no cable)

`ota.py` + `otapush.py` implement staged, sha256-verified WiFi updates. Set
`OTA_TOKEN` (any long random string) in a board's `secrets.py` and deploy once
over USB; from then on the board listens on `OTA_PORT` (default `8266`) and
updates go over WiFi:

```sh
cd firmware/judge-neck
python3 ../micropython/otapush.py <board-ip>              # main.py + shared libs
python3 ../micropython/otapush.py <board-ip> main.py      # one file
```

How it stays safe:

- files stage as `*.new` on the board — running code is untouched until commit
- commit verifies **every** file's size + sha256 before swapping **any**;
  a failed verify or a dropped connection mid-update changes nothing
- `boot.py` is refused outright, and USB (`deploy.sh` / `mpremote`) always
  remains the recovery path if an update goes sideways
- every command carries the token; boards with no `OTA_TOKEN` don't listen

The listener is polled from `wetline.py`'s idle points, so OTA works whether
or not the orchestrator is up. The device resets itself after a successful
commit and rejoins the fleet on the new code.
