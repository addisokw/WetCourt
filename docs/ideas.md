# Ideas

## Manual TTS mode
input field for operators to type arbitrary text in that will be spoken outloud (to draw people in as a puppet mode for example)

## Manual charge input — ✅ implemented
The operator panel should have an interface to queue up charges (manually type in charges and submit for next case).
*(Crimes panel → "queue a charge for the next trial".)*

## Creator mode — ✅ implemented (as category filter)
Special mode that runs charges that are specific to a youtuber.
*(Tag the creator's charges with a category in the crime list, then "draw only from" that category in the Crimes panel.)*

## Verdict drop
A mechanical “GUILTY / INNOCENT” placard that physically drops from above the bench at verdict time. Wile E. Coyote energy. More committed than a screen.

## Air puff output
Cheap solenoid valve and a small compressor fires a puff at the defendant’s face on key moments — when contradicted, when caught lying. Startle response is theatrical and the cleanup is zero. Gives you a gradation between “minor scolding” (puff) and “major sentence” (squirt).

## Swear-in object 
Capacitive sensor in a (rubber chicken / McMaster catalog or similar). Hand must stay on it during testimony; lifting it triggers “the defendant is reluctant to maintain their oath.” Forces engagement physically.

## Directional speakers 
The judge’s voice comes from a mono speaker behind the bust. When the prosecutor talks, audio shifts to the other side of the booth. Spatial separation makes multiple AI characters feel like distinct entities even though they share one LLM.

## Deliberation theater — ✅ implemented 
Lights dim, judge’s eyes pulse, dramatic synth pad plays for 4–6 seconds, then snap back for verdict. Costs nothing, adds enormous gravitas.
Slow-motion verdict reveal. “The court finds the defendant… [3-second beat]… GUILTY.” Music swells, squirt gun fires on the final syllable.

## Thermal-printed receipt
Crime, verdict, sentence, and the LLM-extracted “key quote” from your defense. ~$50 printer, enormous return — people love physical proof of an absurd appearance, and the receipt is what they show their friends later.

## Precedent citation 
Judge references previous cases in real time. “This court recalls Case 4:32, where a defendant made nearly the same argument and was found guilty.” Trivially easy with a running log added to the LLM context. Makes the exhibit feel like an evolving body of jurisprudence rather than isolated trials.

## Audience-sourced charges 
Same QR code lets spectators submit potential charges that get queued up for future defendants. Self-replenishing content.

## Prosecutor AI
A second LLM persona that argues against the defendant. After the defense, the prosecutor gets 10 seconds to dismantle the argument. Suddenly it’s an actual adversarial proceeding. Probably the highest-leverage single addition — it adds dramatic tension and gives the AI something concrete to do.
Public defender AI. For defendants who freeze at the mic, the option to have an AI argue for them. Lets the chronically shy participate, and the persona can be played as overworked and half-hearted for comedy.
Bailiff voice. TTS voice handling “All rise” / “Order in the court” / “The defendant will state their name.” Frees the judge to be the heavy and stages proceedings properly without one AI doing every voice.

## Cross-examination — ✅ implemented
After the defense, the judge asks one pointed follow-up question based on what they actually said, defendant gets 10 seconds to answer. This is where the LLM really earns its keep — it can engage specifically with the weakest part of the argument. Adds maybe 15 seconds and dramatically lifts perceived intelligence.
*(Inserts a question→answer loop between the plea and the verdict; the answer is folded into the deliberation prompt. Operator-toggleable in the console (and `[cross_examination]` in config); skipped automatically when the defendant offered no plea. Any timeout falls through to the verdict so it can't stall a trial.)*

## Call-your-lawyer phone
A real analog phone in the booth. Lift the handset and you're connected to your
court-appointed lawyer — same STT/LLM/TTS stack, played as spectacularly
incompetent. Physical big brother of the public defender idea above.

Hardware: a Grandstream HT801 FXS ATA (~$35) makes any garage-sale phone a SIP
endpoint — it supplies loop voltage, real ring voltage, off-hook detection, and
DTMF, so the phone stays stock. Needs ethernet + power at the phone's spot.
Far cleaner than gutting the handset onto a USB sound card (hook switch on
GPIO, no way to ring).

Software: nothing in the stack speaks SIP, so add two containers on the Spark —
Asterisk as registrar/media anchor, with **AudioSocket** forking call audio to
a TCP socket as raw 8 kHz PCM, and a small "lawyer-agent" sidecar (~200 lines
of Python) running the loop: VAD-endpointed record → `/v1/audio/transcriptions`
→ `/v1/chat/completions` (lawyer persona) → `/v1/audio/speech` → resample
24k→8k → play, until on-hook. Same LiteLLM endpoints, zero new inference infra;
the orchestrator is untouched and the bit degrades independently. VAD
endpointing is the only genuinely new component (booth mic is push-to-talk).

Details that make it sing:
- **Hotline mode**: HT801 "Offhook Auto Dial" — lift handset, lawyer answers.
  No keypad needed.
- **The phone can ring**: Asterisk can originate a call *to* the phone, so
  "your lawyer is calling YOU" can fire mid-trial. Possibly the best gag here.
- **Latency is in character**: ~4 s per turn is dead air on a phone, but hold
  music / keyboard clatter / "pulling up your file, system's slow today"
  covers it diegetically.
- **The phone line is the audio filter**: 8 kHz narrowband + handset speaker
  gives the "voice on a phone" aesthetic free — use a clean Kokoro voice,
  distinct from the judge's robot-processed one. Parakeet already resamples
  incoming audio, so 8 kHz caller audio needs no changes.
- **DTMF works**: terrible IVR on the table. "Press 1 if guilty. Press 2 if
  very guilty."
- **Trial context**: sidecar reads the current charge from the orchestrator
  (read-only) so the lawyer gives specifically bad advice about *your* crime.

Honest cost: the ATA is the easy part; the work is ~a day of Asterisk config
grief (AudioSocket keeps the surface tiny) plus the sidecar loop.

## Post processing glitchiness filters on audio TTS — ✅ implemented
A robot-aesthetic Web Audio chain applied to all TTS playback (`frontend/src/robot.ts`):
every PCM chunk routes through a persistent graph — bandpass/peak EQ → soft-clip
saturation → ring modulation (~52 Hz carrier) → comb resonance, wet/dry blended —
then an AudioWorklet tail (`glitch-processor.js`) adds bitcrush, sample-rate
decimation, and occasional stutter/dropout glitches. Uniform across personas,
continuous across chunk seams. A live operator-console slider ("Robot", 0–100%)
scales the whole effect by ear and persists to localStorage; since only the
operator `/ws` client plays PCM, it's a local audio control with no backend
round-trip. Per-effect tuning lives at the top of `robot.ts` and in the
worklet's `parameterDescriptors`.
