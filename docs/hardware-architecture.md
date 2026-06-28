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

- **AI judge** — LED-matrix face (Adafruit Matrix Portal M4 / SAMD51) and a
  pan/tilt gaze mechanism.
- **Squirt-gun turret** — a NanoC6 driving a pan/tilt mech + a relay to fire
  (`firmware/turret/`), with a camera on top for person-tracking and a
  manual-aim feed (the **vision** process, `vision/`).
- **Gavel** — a NanoC6 driving a servo-actuated gavel (verdicts, and "order in
  the court").
- **Swear-in object** *(future, not started)* — a capacitive swear-in object on
  its own micro that triggers the start of a trial (and possibly oath-presence
  sensing later).
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
│   ├── ai-judge/          #   Matrix Portal M4 (SAMD51): LED face + pan/tilt gaze
│   ├── gavel/             #   M5 NanoC6 (esp32c6): servo gavel
│   ├── turret/            #   M5 NanoC6 (esp32c6): pan/tilt + relay (FIRE, turret AIM)
│   ├── swear-in/          #   (future) capacitive swear-in object: emits the start trigger
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
| AI judge (face + gaze) | Adafruit Matrix Portal M4 (SAMD51) | `PANEL`, gaze `AIM` | `firmware/ai-judge/` |
| Gavel | M5Stack NanoC6 (esp32c6) | `GAVEL` | `firmware/gavel/` |
| Squirt-gun turret | NanoC6 + camera | `FIRE`, turret `AIM`; tracking | `firmware/turret/` + `vision/` |
| Swear-in object *(future)* | TBD micro | `BUTTON` (start trigger) | `firmware/swear-in/` |

`LIGHTS` is deferred (no owner); e-stop is operator-panel + hardware power, not
a device.

## What changes in the orchestrator

The hardware layer goes from **one connection → a device registry**:

- `tcp.rs` accepts **N** simultaneous connections instead of one. Each device,
  on connect, identifies itself with `HELLO <role>` (see the protocol spec).
- The orchestrator keeps a registry of `role → connection` and **routes** each
  `HardwareCommand` to the device that owns that verb (e.g. `FIRE` → `turret`,
  `GAVEL` → `gavel`, `PANEL` → `ai-judge`).
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
   (e.g. `gavel/`) and add `firmware/README.md` board map.
3. **When the first device connects:** add the `HELLO <role>` handshake + the
   device registry + per-role command routing in `orchestrator/src/hardware/`,
   and drop the dead `ESTOP` reader branch from `tcp.rs`.
4. **When the turret lands:** add `firmware/turret/` (firmware) and `vision/`
   (camera process); wire the vision channel into the operator console.

Nothing above is big-bang; each step is independent and tied to a real need.

## Open items (need your call)

- **Swear-in object micro** — board choice, and whether it also does oath /
  presence sensing (it currently only owns the start trigger). Not started.
