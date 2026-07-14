# Hardware architecture & repo structure

Status: **agreed direction** (2026-06) — staged migration, executed as each
piece of hardware actually lands. Captures how the project goes from one
all-in-one microcontroller to a fleet of independent devices plus a vision
process, and how the directories grow to match.

## Context

The project started with a single all-in-one M5Stack NanoC6 (esp-idf Rust)
that dialed the orchestrator over TCP and owned every verb. **That firmware is
retired** (removed in this change): development has moved off Rust for firmware,
and its responsibilities are being redistributed across independent devices.

The build now splits into several independent subsystems, each with its own
firmware and microcontroller, plus a non-MCU vision process:

- **AI judge** — **two** boards: `firmware/judge-face/` drives the LED-matrix
  face (Adafruit Matrix Portal M4 / SAMD51, `PANEL`), and `firmware/judge-neck/`
  drives the pan/tilt gaze (NanoC6 + 8-servo, `AIM`). They're split because the
  HUB75 panel + Protomatter timing fully occupy the Matrix Portal, so the gaze
  reuses the turret's NanoC6 + 8-servo recipe on its own board.
- **Squirt-gun turret** — **two** NanoC6 boards: `firmware/turret/` drives the
  pan/tilt mech (`AIM`), and `firmware/squirt/` drives the firing relay (`FIRE`).
  They're split because the servo board claims the NanoC6's only Grove I2C pins,
  leaving no GPIO for the relay. A camera on the gun feeds the **vision** process
  (`vision/`) for person-tracking and a manual-aim feed.
- **Gavel** — a NanoC6 driving a servo-actuated gavel (verdicts, and "order in
  the court").
- **Swear-in button** — the defendant's arcade button (built-in lamp) on its
  own NanoC6: a press starts the trial or serves as a non-verbal
  acknowledgement; the host drives the lamp (`LED`) to cue when a press means
  something.
- More to come, each as an independent firmware set.

Two things are intentionally **out of scope** for now:

- **No hardware e-stop.** Emergency stop is handled from the operator panel
  (`/operator/estop`), backed by physical hardware power shutdowns. No firmware
  emits an `ESTOP` event.
- **No lights.** `LIGHTS` is deferred — no device currently drives it. It may
  return once the rest of the hardware is complete.

## Decisions

1. **Firmware boundary:** all first-party firmware lives in-repo under
   `firmware/<board>/`, the turret included (`firmware/turret/`).
2. **Vision/camera:** a non-MCU host process in **`vision/`**, coupled to the
   turret hardware. The orchestrator talks to it over the network (its own
   HTTP/WS channel, not the device line protocol).
3. **Wire protocol:** a **language-neutral spec** in [`protocol/`](../protocol/),
   implemented by the orchestrator and every firmware (esp-rs, CircuitPython,
   Arduino, …). See that doc for the contract.

## Target structure

```
WetCourt/
├── orchestrator/          # brain + operator UI. Owns the DEVICE REGISTRY + protocol host
├── firmware/              # one self-contained project per independently-flashed board
│   ├── judge-face/        #   Matrix Portal M4 (SAMD51): LED-matrix face — PANEL
│   ├── judge-neck/        #   M5 NanoC6 (esp32c6) + 8-servo: pan/tilt gaze — AIM
│   ├── gavel/             #   M5 NanoC6 (esp32c6): servo gavel
│   ├── turret/            #   M5 NanoC6 (esp32c6): pan/tilt servos — AIM
│   ├── squirt/            #   M5 NanoC6 (esp32c6): firing relay — FIRE
│   ├── swear-in/          #   M5 NanoC6 (esp32c6): defendant's arcade button — LED; emits BUTTON
│   └── README.md          #   board map: subsystem → board → MCU → verbs owned
├── vision/                # non-MCU host process: turret camera person-tracking + aim feed
├── protocol/              # language-neutral device⇄orchestrator wire spec (versioned)
├── dgx-ai-stack/          # inference stack
├── deploy/  docs/  README.md
```

`firmware/` is a **container of islands, not a shared build** — the boards span
different MCUs, toolchains, and languages (SAMD51 Matrix Portal, esp32c6 NanoC6;
firmware is no longer Rust), so they don't share a workspace. The unifying
artifacts are the board-map README and the protocol spec, not a build. Adding
hardware = a new sibling dir + one registry entry + a role in the spec.

## Board map

| Subsystem | Board / MCU | Owns (verbs / events) | Lives in |
|---|---|---|---|
| Judge face | Adafruit Matrix Portal M4 (SAMD51 + AirLift) | `PANEL` | `firmware/judge-face/` |
| Judge neck (gaze) | M5Stack NanoC6 (esp32c6) + 8-servo | gaze `AIM` | `firmware/judge-neck/` |
| Gavel | M5Stack NanoC6 (esp32c6) | `GAVEL` | `firmware/gavel/` |
| Turret (aim) | M5Stack NanoC6 (esp32c6) + camera | turret `AIM`; tracking | `firmware/turret/` + `vision/` |
| Squirt (fire) | M5Stack NanoC6 (esp32c6) | `FIRE` | `firmware/squirt/` |
| Swear-in button | M5Stack NanoC6 (esp32c6) | `LED` (emits `BUTTON`) | `firmware/swear-in/` |

`LIGHTS` is deferred (no owner); e-stop is operator-panel + hardware power, not
a device.

## What changes in the orchestrator

The hardware layer goes from **one connection → a device registry**:

- `tcp.rs` accepts **N** simultaneous connections instead of one. Each device,
  on connect, identifies itself with `HELLO <role>` (see the protocol spec).
- The orchestrator keeps a registry of `role → connection` and **routes** each
  `HardwareCommand` to the device that owns that verb (e.g. `FIRE` → `turret`,
  `GAVEL` → `gavel`, `PANEL` → `judge-face`).
- `HardwareCommand` gains a target (or each verb maps to a role); acks/timeouts
  stay per-command but are tracked per-connection.
- A command whose target device is absent degrades gracefully (log + skip),
  matching the existing "never stall in front of a visitor" stance.
- The current `tcp.rs` reader parses an `ESTOP` line into
  `Event::OperatorEmergencyStop`; that branch is dropped (no device emits it).
  The operator-panel `/operator/estop` path is unchanged.

The **vision process** (`vision/`) talks to the orchestrator over its existing
HTTP/WS server — tracking state in, and the aim feed surfaced as an
operator-console panel (MJPEG/WebRTC). The turret's *firmware*
(`firmware/turret/`) still speaks the line protocol like any other device; the
*vision* is a separate channel.

## Staged migration (pay as the hardware arrives)

1. **Now:** add `protocol/` spec and this doc; **retire the old Rust firmware**
   (`firmware/` removed). ✅
2. **When the first new board's firmware starts:** create `firmware/<role>/`
   and add `firmware/README.md` board map. ✅ (`firmware/turret/`)
3. **When the first device connects:** add the `HELLO <role>` handshake + the
   device registry + per-role command routing in `orchestrator/src/hardware/`,
   and drop the dead `ESTOP` reader branch from `tcp.rs`. ✅
   (`tcp.rs` is now the multi-device registry; verified end-to-end against a
   socket — handshake, presence, calibrated AIM routing, disconnect cleanup.)
4. **When the turret lands:** add `firmware/turret/` (firmware ✅) and `vision/`
   (camera process — pending, Phase B); wire the vision channel into the
   operator console.

Nothing above is big-bang; each step is independent and tied to a real need.

## Open items (need your call)

- **Swear-in button semantics** — board is settled (NanoC6, arcade button on
  G1 + lamp on G2; firmware in `firmware/swear-in/`). Open: which trial states
  treat a press as a non-verbal acknowledgement (vs. ignore it), and the
  per-state lamp cue map.
