# counsel — the call-your-lawyer phone

SIP endpoint + AI voice agent for the booth's analog phone (Grandstream
HT801 ATA). Lift the handset → an intake menu ("press 1 if guilty, press 2
if very guilty") → Dewey Stubb, Esq. gives specifically terrible advice
about the live charge. The operator console can also ring the phone
mid-trial (`/lawyer/call`).

Full design, HT801 provisioning checklist, and bring-up order:
[`docs/lawyer-phone.md`](../../../docs/lawyer-phone.md).

## Run

```bash
cd orchestrator
cargo run -p counsel -- --config crates/counsel/config.dev.toml

# offline (no Spark): canned lawyer, tone TTS
COUNSEL__INFERENCE__MODE=mock cargo run -p counsel -- --config crates/counsel/config.dev.toml

# media-path echo diagnostic
COUNSEL__AUDIO__ECHO_TEST=true cargo run -p counsel -- --config crates/counsel/config.dev.toml
```

Any config field is overridable via `COUNSEL__SECTION__FIELD` env vars.
`LITELLM_MASTER_KEY` from `.env` / `dgx-ai-stack/.env` is aliased to
`COUNSEL__INFERENCE__API_KEY` automatically.

## Layout

- `src/sip/` — rsipstack endpoint: registrar (the ATA registers here), UAS
  (answers off-hook auto-dial), UAC (ring-out)
- `src/rtp/` — hand-rolled media: G.711 µ-law, 20 ms paced sender with a
  priority mixer (speech > cover loop > silence), RFC2833 DTMF, 24 kHz→8 kHz
  FIR decimator
- `src/audio/` — energy VAD endpointing, WAV wrap for STT, cover-asset loader
- `src/call/` — call slot (single line), the agent turn loop, IVR gag,
  trial-context fetch
- `src/inference.rs` — trimmed copy of the orchestrator's LiteLLM client
  (deliberately duplicated so the phone degrades independently)
- `personas/lawyer.toml` — the whole character: prompt, greeting, reprompt,
  sign-off, fallback lines, Kokoro voice

## Regenerating assets

All assets are 8 kHz mono s16 WAV; missing files degrade to silence.

IVR prompt (any Kokoro voice; then downsample):

```bash
source ../dgx-ai-stack/.env
curl -s http://<spark>:4000/v1/audio/speech \
  -H "Authorization: Bearer $LITELLM_MASTER_KEY" -H "Content-Type: application/json" \
  -d '{"model":"kokoro-tts","voice":"af_sarah","input":"You have reached the law offices of...","response_format":"wav"}' \
  -o /tmp/ivr_24k.wav
ffmpeg -i /tmp/ivr_24k.wav -ar 8000 -ac 1 -sample_fmt s16 assets/ivr_prompt_8k.wav
```

`keyboard_clatter_8k.wav` and `hold_music_8k.wav` are synthesized
placeholders — replace with real recordings via the same ffmpeg incantation
whenever the mood strikes.

## Tests

```bash
cargo test -p counsel   # unit: g711, dtmf, resampler, vad, sdp, wav, config
```

Integration is scripted SIP/RTP — no softphone needed. With counsel running
(mock mode unless noted), from anywhere:

```bash
python3 crates/counsel/scripts/sip_echo_test.py    # REGISTER/INVITE/RTP echo/DTMF/486/BYE (needs ECHO_TEST=true)
python3 crates/counsel/scripts/mock_convo_test.py  # greeting → VAD turn → reply
python3 crates/counsel/scripts/ivr_test.py         # intake menu + DTMF cut-through
python3 crates/counsel/scripts/ringout_test.py     # POST /call rings a scripted phone
python3 crates/counsel/scripts/real_convo_test.py  # full stack vs the Spark (real mode;
                                                   # regenerate scripts/caller.wav via Kokoro first)
```

Or point any PCMU-capable softphone (Linphone) at `<host>:5060` and dial `1`.
`scripts/make_assets.py` regenerates the synthesized cover assets.
