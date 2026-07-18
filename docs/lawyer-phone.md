# Call-your-lawyer phone

A real analog phone in the booth. Lift the handset → connected to Dewey
Stubb, Esq., a spectacularly incompetent AI defense attorney running on the
same local STT/LLM/TTS stack as the judge. Mid-trial, the operator can also
make **the phone ring** ("your lawyer is calling YOU").

```
garage-sale phone ──FXS──▶ HT801 ATA ──SIP/RTP (UDP, PCMU 8k)──▶ counsel ──HTTP──▶ LiteLLM :4000
   (stock, analog)      192.168.50.216                              │:8092            (STT/chat/TTS)
                                                                    │
    operator console ──▶ orchestrator :8080 ── /lawyer/* proxy ────▶│
                                  ▲──── GET /trial/state ◀──────────┘  (live charge → case file)
```

The service is **`counsel`** (`orchestrator/crates/counsel/`): a single Rust
binary speaking SIP via [rsipstack] with a hand-rolled RTP/G.711 media layer.
No Asterisk. The trial loop never depends on it — if counsel dies, the phone
is just dead, which is honestly also in character for this firm.

[rsipstack]: https://github.com/restsend/rsipstack

## Running it

```bash
# dev (same LAN as the HT801; inference on the Spark over Tailscale)
cd orchestrator
cargo run -p counsel -- --config crates/counsel/config.dev.toml

# offline dev without the Spark: canned lawyer, tone TTS
COUNSEL__INFERENCE__MODE=mock cargo run -p counsel -- --config crates/counsel/config.dev.toml

# media-path diagnostic: answers calls with a raw echo instead of the lawyer
COUNSEL__AUDIO__ECHO_TEST=true cargo run -p counsel -- --config crates/counsel/config.dev.toml
```

The LiteLLM key is picked up from `dgx-ai-stack/.env` (`LITELLM_MASTER_KEY`)
exactly like the orchestrator dev loop. Control plane:

- `GET :8092/health` — liveness
- `GET :8092/status` — registration + call state (proxied at `/lawyer/status`)
- `POST :8092/call {"reason": "..."}` — ring the phone; blocks until
  answered / 25 s no-answer (proxied at `/lawyer/call`; console → Lawyer tab)

## HT801 provisioning checklist

Web UI at `http://192.168.50.216` (give it a DHCP reservation). Settings that
matter, per tab:

**Basic Settings**
- Web/keypad password: set them, it's a public booth.

**FXS Port**
- Primary SIP Server: `<counsel host IP>:5060` — no outbound proxy.
- NAT Traversal: **No**; STUN server blank (flat LAN, counsel latches
  symmetric RTP anyway).
- SIP User ID / Auth ID: `defendant`; password: set one (stored but not
  challenged in v1 — counsel trusts the LAN).
- SIP Registration: **Yes**; Register Expiration: **2 minutes** (fast
  recovery after reboots/moves).
- **Offhook Auto-Dial: `1`**, Offhook Auto-Dial Delay: `0` — lift handset,
  lawyer answers. No keypad needed.
- Preferred Vocoder: choices 1–8 **all PCMU** (lock the codec).
- DTMF: **via RTP (RFC2833)** payload type 101; disable in-audio DTMF and
  SIP INFO.
- Disable: call waiting, call features (hook-flash transfer etc.), 3-way
  calling, and **Session Expiration / session timers** (avoids re-INVITE
  paths counsel doesn't speak).
- Use Random RTP Port: **No**; Local RTP Port: `5004`.
- "Allow Incoming SIP Messages from SIP Proxy Only": **Yes** (the belt to
  the no-digest suspender).

Then: reboot the ATA, watch `counsel` log `registered user=defendant`, lift
the handset.

Software-only regression (no ATA needed): the scripted SIP/RTP clients in
`orchestrator/crates/counsel/scripts/` cover echo, the conversation loop,
the IVR menu, and ring-out — see the crate README for the run matrix.

**No hardware at all?** `scripts/softphone.py` makes your computer the phone
(mic + speakers over real SIP/RTP; `pip install sounddevice`), so you can
experiment with Dewey anywhere. Every call — softphone, scripted, or real
ATA — is recorded under `recordings/` as a stereo WAV (caller + lawyer
overlaid) plus a JSON transcript timeline, ready to share with collaborators.
See the crate README's "Testing without the phone hardware" and "Recordings"
sections.

## Bring-up order (first time on real hardware)

1. `COUNSEL__AUDIO__ECHO_TEST=true`, lift handset, talk — you should hear
   yourself ~40 ms later. Proves SIP + RTP + codec both ways.
2. If anything is off, capture the first call:
   `sudo tcpdump -i <iface> -w /tmp/ht801.pcap udp port 5060 or udp portrange 40000-40099`
   and read it with `sngrep`/Wireshark. The SDP parser was written against
   the HT801's offer shape; a pcap shows any surprise instantly.
3. Echo off, mock mode on — full call flow (IVR gag, DTMF, turn-taking)
   without inference.
4. Real mode. Tune VAD on the actual handset:
   `COUNSEL__AUDIO__DEBUG_RMS=true` logs per-frame RMS while listening.
   Watch a quiet line vs. speech and set `[audio] vad_rms_threshold`
   comfortably between (default 700, i16 scale). If the booth is loud,
   raise it; if the lawyer talks over shy defendants, lower it.
5. Ring-out: console → Lawyer tab → "Call the defendant's lawyer." The
   HT801 generates real ring voltage; garage-sale bells ring.

## Config knobs that earn their keep

| Knob | Default | Why you'd touch it |
|---|---|---|
| `[sip] advertise_ip` | auto-detect | multi-homed host or the detection picks the wrong interface |
| `[audio] vad_rms_threshold` | 700 | handset/booth noise floor (see debug_rms) |
| `[audio] silence_reprompt_secs` | 12 | how long Dewey waits before "hello? that's how they get you" |
| `[audio] max_call_secs` | 300 | line hog control; ends with an in-character sign-off |
| `[audio] max_exchanges` | 5 | back-and-forths before a scripted mishap (`hangup_lines`) forces Dewey off the line |
| `[audio] tts_gain` | 1.0 | linear gain on Dewey's voice (soft-clipped) for quiet handsets |
| `[audio] booth_mirror` | true | stream Dewey's side of the call to the orchestrator so the booth speakers carry it too |
| `[trial_context] enabled` | true | lawyer reads the live charge from the orchestrator |
| `[persona] file` | personas/lawyer.toml | swap the whole persona |

## Assets

`orchestrator/crates/counsel/assets/` — all 8 kHz mono s16 WAV, µ-law-encoded
at startup, missing files degrade to silence:

- `ivr_prompt_8k.wav` — intake menu ("press 1 if guilty, press 2 if very
  guilty"); regenerate with a Kokoro call + downsample, see
  `crates/counsel/README.md`
- `keyboard_clatter_8k.wav` — played while STT/LLM/TTS run ("pulling up your
  file"), so inference latency reads as office ambience
- `hold_music_8k.wav` — plays under the IVR keypress window, then the post-IVR
  hold: music broken by the office voice announcing your queue position ("you
  are number twenty-three million… in line for one of our award-winning senior
  partners… you are next in line for our most available attorney"). The line
  template and office voice live in the persona (`hold_line` / `hold_voice`,
  `{n}` = fresh random number per call); delete `hold_line` or the asset to
  skip the hold entirely.

Replace any of them with real recordings at will:
`ffmpeg -i in.wav -ar 8000 -ac 1 -sample_fmt s16 out_8k.wav`.

## Known limits (v1, by design)

- No barge-in: the lawyer cannot be interrupted mid-sentence (fits the bit).
- No registrar digest challenge: the LAN is the trust boundary.
- One line: a second caller gets 486 Busy Here; ring-out while busy → 409.
- The lawyer's advice does not improve.
