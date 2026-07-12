# Booth backdrop: the thermal-strip wall

Design notes for the OpenSauce booth backdrop. Instead of a printed vinyl
banner, the rear chain-link fence of the 8×8 ft booth is covered in long
thermal-printer strips — printed in-house on the same printer that produces the
trial keepsakes, run top-of-booth to floor, overlapping, zip-tied to the fence.
The backdrop is itself an artifact of the exhibit.

Status: **idea, not started.** Captured from a brainstorm on 2026-07-10/11 so
we can pick it back up. The keepsake renderer this would build on is documented
in [`thermal-printer.md`](thermal-printer.md).

## Physical setup

- Booth: 8×8 ft, table at front, large chain-link fence forming the rear wall.
  The fence already reads as "holding cell / evidence cage" — hang in-world
  content on it, don't hide it.
- Strips: 80 mm thermal paper (576 dots printable @ ~203 dpi), each ~8 ft
  (~2.4 m ≈ ~19,500 dots) long.
- Coverage: ~40 strips at ~60 mm effective spacing (80 mm strips, overlapped)
  covers the 8 ft width ≈ 100 m of paper ≈ two standard 80 m rolls. Batch-print
  the whole wall the night before.
- Rigging: zip-tie top **and** bottom of every strip (thermal paper curls hard
  toward its roll). Punch holes and reinforce with a tab of packing tape;
  or zip-tie a washer / short dowel into the bottom hem as a weight so strips
  don't flap print-side-backward in crowd draft.
- Fade: direct sun + heat grays thermal print over a weekend. Either accept it
  ("evidence degrades in custody") or print a refresh batch for day two —
  at this cost, disposability is a feature.
- Overlap-proofing: put the seal + masthead at the top of every strip so
  wherever strips overlap, whatever peeks out still looks intentional.

## Design principle: two visual scales

A receipt strip has two audiences: someone 30 ft away deciding whether to walk
over, and someone 2 ft away waiting in line. Standard receipt text is invisible
past ~6 ft, so the wall is designed in two layers:

- **Far layer** — a *thermal mural*: one large image sliced into 576-dot-wide
  vertical columns, one column per strip, reassembled side-by-side on the
  fence. Doubles as the booth "banner."
- **Near layer** — readable content strips interleaved around the mural, for
  the people up close.

And within the mural itself there are two content decisions, not one:

1. **Macro subject** — what you see from 30 ft.
2. **Micro substance** — what the dark pixels are *made of* up close. Thermal
   is 1-bit; every "gray" is texture, and the texture can be more content
   instead of meaningless dither.

## Mural: macro subject candidates

- **The court seal, engraved like currency.** We already draw the seal
  procedurally. Scaled to ~5 ft in engraving style — concentric line work,
  radial hatching, banner ribbons (*THE WET COURT OF APPEALS · IN AQUA
  VERITAS*). Reads as "official institution" from across the hall, which is
  the joke. Line art prints crisply where photo dithers turn to mush.
- **Lady Justice, armed.** Blindfold, scales in one hand, super soaker in the
  other; woodcut / courtroom-sketch line style. The whole exhibit in one image.
- **The GUILTY stamp.** Enormous distressed rubber-stamp letters at a slight
  angle with a splash bursting through. The most photographed word in the
  booth.
- **Giant vertical letterforms.** One huge rotated letter per strip spelling
  THE WET COURT OF APPEALS — the most literal banner replacement.
- **The mugshot wall** (different genre — functional, not pictorial). Strips
  printed as a police-lineup *height chart*: bold horizontal lines with ft/in
  markings spanning the fence, masthead *WET COURT DEPT. OF CORRECTIONS*.
  Turns the backdrop into a photo op — defendants hold their keepsake like a
  booking placard and take a mugshot. Every share advertises the booth.

## Mural: micro substance candidates

- **Charges as text-halftone** (favorite). Typeset the 218 charges in tiny
  continuous print, modulating weight/density by the artwork's luminance —
  dark regions dense bold text, light regions sparse. Far: Lady Justice /
  the seal. Close: "the defendant stands accused of typing 'lol' with a
  completely straight face." Fuses the far and near layers into one artifact.
- **Legal boilerplate.** Same technique, substance is the fake Wet Code /
  Miranda parody — endless § numbers forming the image.
- **Clustered-dot halftone.** No hidden content, but chunky newspaper-style
  dots keep contrast at distance and read as vintage newsprint. (Fine
  error-diffusion dither like Floyd–Steinberg averages to flat gray from far
  away — avoid for the mural.)
- **Micro-iconography.** Halftone cells as tiny gavels/droplets. Cute, but
  reads as noise unless cells are large; ranked below text-halftone.

## Near-layer strip content

- **The docket.** The full 218-charge list, a few charges per strip, under a
  "TODAY'S DOCKET" masthead with the seal. Self-incriminating by design —
  people in line read the charges and recognize themselves, which is exactly
  the state of mind to step up in.
- **The living casebook.** Print each trial's keepsake **twice** — one for the
  defendant, one zip-tied to the fence. The wall starts the day sparse and
  accumulates real verdicts, pleas, and guilty blast photos as the show runs.
  Social proof that builds itself. (Implementation: the print service renders
  every `TrialRecord` twice — roughly a one-line change in
  `src/printer/service.rs`.)
- **Legal-boilerplate texture.** Wet Code of Civil Procedure with escalating
  section numbers ("§ 404.1 — Plea Not Found"); a Miranda parody that sneaks
  the tech pitch in ("You have the right to remain dry. Anything you say can
  and will be transcribed locally, on-device, with no cloud round-trip…");
  a terms-and-conditions scroll running all the way to the floor (pairs with
  charge #2, claiming to have read them).
- **Redaction strips.** Fake evidence transcripts with heavy black redaction
  bars. Solid black prints beautifully and reads from a distance — doubles as
  far-layer texture. "EXHIBIT ██: the defendant's ███████ was found ██████."
- **Caution-tape strips.** Repeating diagonal hazard pattern, magnified type:
  **SPLASH ZONE — PLEAD AT YOUR OWN RISK.** Frames the mural edges or marks
  the real splash zone at the floor line.

## Recommended combo

Engraved seal *or* Lady Justice as the centerpiece, rendered as
**text-halftone from the charge list**; mugshot height-chart strips on one
side of the fence; docket strips at eye level; the living casebook filling in
over the day. Centerpiece pulls people in, height chart converts them into
shareable photos, microtext rewards the line-waiters.

## Rendering pipeline (to build)

Small standalone tool (Python or a bin in the `thermal-printer` crate):

1. Render or ingest artwork at full mural resolution (a 30-strip mural is
   ~17,000 × ~19,500 dots — plenty of headroom; microtext at 6–8 px x-height
   is legible at receipt-reading distance).
2. **Contrast discipline:** posterize to 2–3 tones before halftoning; keep
   large solid-black and solid-white regions. Murals fail at distance from
   timid mid-grays, not low resolution. Line art / typography barely needs
   dithering at all.
3. Apply the text-halftone or clustered-dot pass.
4. Slice into 576-dot columns; emit one ESC/POS job per strip.
5. Print **alignment tick marks at fixed heights on strip edges** so 30 strips
   can be zip-tied level on a chain-link fence, plus a strip index number in a
   corner so the print order survives the pile on the workbench.

First milestone: proof a single strip of the seal-as-charge-text render on the
real printer to validate microtext size, halftone cell size, and blackness
before committing to the full wall.

## Open questions

- Which macro subject for the centerpiece (seal vs. Lady Justice vs. GUILTY)?
- Indoor or outdoor booth placement — how bad will fade be?
- Does OpenSauce booth rigging allow zip ties to the fence top rail?
- One mural centered, or mural + height chart split (recommended)?
