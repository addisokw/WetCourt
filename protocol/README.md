# Wet Court device protocol

The wire contract between the **orchestrator** (host) and each **device**
(microcontroller firmware). Language-neutral on purpose: the orchestrator is
Rust, but devices span esp-rs, CircuitPython, and Arduino — they all implement
*this* doc.

Spec version: **2** — the multi-device protocol (`HELLO` identity handshake +
per-role routing). v1 was the original single all-in-one MCU with no handshake;
that firmware is retired, so no v1 devices remain.

## Transport & framing

- **TCP.** The **device dials the orchestrator** (the host is the server,
  default `:8090`); devices connect over WiFi. The host never dials out.
- **Lines.** ASCII, one message per line, `\n`-terminated (`\r` tolerated).
  Tokens are whitespace-separated; the first token is the verb (uppercase).
- Blank lines are ignored. Unknown lines are logged and skipped (forward-compat).

## Connection & identity  *(v2)*

On connect, before anything else, the device announces its role:

```
→  HELLO <role> [<fw-version>]        device → host
←  WELCOME                            host → device   (accepted)
←  BYE <reason>                       host → device   (rejected; host closes)
```

`<role>` is one of the roles below. The host keeps a registry of
`role → connection` and routes commands to the device that owns each verb. A
second connection claiming a live role replaces the stale one.

`BYE <reason>` is sent, and the connection closed, when the handshake fails:

| `<reason>` | Cause |
|---|---|
| `bad_hello` | First line wasn't `HELLO <role>`, or didn't arrive within the handshake timeout. |
| `unknown_role` | `<role>` isn't a known role. |
| `bad_version` | Firmware major is incompatible (reserved; not yet enforced). |

Role tokens are case-sensitive. The canonical spelling is the hyphenated form in
the table below (`judge-neck`); the host also accepts the underscore form
(`judge_neck`) it uses internally for the JSON API and calibration filenames.

### Roles

| Role | Subsystem | Verbs it must accept |
|---|---|---|
| `judge-face` | LED-matrix judge face | `FACE`, `AUDIO`, `PERSONA`, `PANEL` *(legacy)*, `PING` |
| `judge-neck` | judge-head pan/tilt gaze | `AIM`, `PING` |
| `gavel` | servo gavel | `GAVEL`, `GJOG`, `PING` |
| `turret` | squirt-gun pan/tilt aim | `AIM`, `PING` |
| `squirt` | squirt-gun firing relay | `FIRE`, `PING` |
| `swear-in` *(future)* | capacitive start trigger | `PING` (emits `BUTTON`) |

Two subsystems are split across two boards each. `turret` and `squirt`: the
servo board claims the NanoC6's only I2C-capable Grove pins for pan/tilt,
leaving no GPIO for the firing relay, so the relay gets its own board. The judge
head, `judge-face` and `judge-neck`: the LED matrix runs on an Adafruit Matrix
Portal M4 (which the HUB75 panel + Protomatter timing fully occupy), while the
gaze pan/tilt reuses the turret's proven NanoC6 + 8-servo recipe on its own
board.

New roles are added here first, then implemented.

## Commands (host → device)

Every command is acknowledged (see Acks). `<...>` are required args.

| Line | Role(s) | Meaning |
|---|---|---|
| `FIRE <ms>` | squirt | Fire the squirt gun for `<ms>` milliseconds. |
| `AIM <pan> <tilt>` | turret, judge-neck | Point the pan/tilt mech (degrees or device-defined units). |
| `GAVEL [<rest> <raise> <strike> <raise_dwell_ms> <strike_dwell_ms> <settle_dwell_ms>]` | gavel | One gavel strike. The host normally sends all six tunables (servo µs positions + dwell ms, from `gavel.toml`) so the firmware stays stateless; a bare `GAVEL` uses the firmware's compiled defaults. |
| `GJOG <us>` | gavel | Move the gavel servo to a raw pulse-width (µs) and hold — live position preview for console tuning. |
| `FACE <phase>` | judge-face | Set the eye/face phase (see vocab). Supersedes `PANEL`. |
| `AUDIO <level>` | judge-face | Live mic envelope, `0.0`–`1.0`; stream at ~20–30 Hz while `listening` (drives pupil dilation). Acked like any command. |
| `PERSONA <name>` | judge-face | Switch the judge's visual persona (see vocab). |
| `PANEL <pattern>` | judge-face | *Legacy* face animation (see vocab); kept while the host migrates to `FACE`. |
| `LIGHTS <state>` | *(deferred — no owner)* | Booth lighting. Not currently driven by any device; may return later. |
| `PING` | any | Keepalive; acknowledged with `OK PING`, like any other command. |

### Vocabularies

- `LIGHTS <state>`: `splash_idle` · `splash_arming` · `guilty` · `not_guilty`
- `FACE <phase>`: `idle` · `listening` · `deliberating` · `verdict:guilty` ·
  `verdict:innocent`
- `PERSONA <name>`: `honorable` · `magistrate` · `cosmic` · `nullpointer` ·
  `petunia`
- `PANEL <pattern>` *(legacy)*: `idle` · `thinking` · `verdict`. The firmware
  maps these onto `FACE` phases: `idle`→`idle`, `thinking`→`deliberating`,
  `verdict`→`verdict:guilty`.

`LIGHTS`/`PANEL` mirror the orchestrator's `LightState` / `PanelPattern` —
extend in both places together. `FACE`/`AUDIO`/`PERSONA` are spec'd
firmware-first (the judge-face CircuitPython eye implements them); the
orchestrator still emits only `PANEL` today and should migrate to `FACE`.

## Acks (device → host)

Exactly one per command, so the host can confirm/time out each action:

```
←  OK <verb>                          executed
←  ERR <verb> <reason>                failed
```

The host applies a per-command ack timeout; a timeout is treated as an error
for that command but does not drop the connection.

## Unsolicited events (device → host)

Sent any time, not in reply to a command:

| Line | Meaning |
|---|---|
| `BUTTON` | Start trigger (capacitive swear-in object) → begins a trial. |

`PING` is acknowledged with `OK PING` (not a separate `PONG`), so every command
resolves through the same one-ack-per-command path; the host tolerates a stray
`PONG` line but devices should ack normally.

There is no hardware e-stop event: emergency stop is driven from the operator
panel (`/operator/estop`) and backed by physical power shutdowns.

## Versioning & compatibility

- The spec carries a single integer **spec version** (currently `2`). Bump it
  for any breaking change to framing, the handshake, or ack semantics.
- Devices report their own firmware version in `HELLO`; the host logs it and
  may refuse incompatible majors with `BYE`.
- The original single-MCU v1 firmware is retired; no legacy fallback is
  provided. Every device sends `HELLO`.

## Implementations

- Host: `orchestrator/src/hardware/` (`protocol.rs` serialises commands;
  `tcp.rs` is the multi-device registry — `HELLO` handshake, per-connection ack
  matching, per-role routing).
- Devices: each `firmware/<role>/` project (the turret included, `firmware/turret/`).
