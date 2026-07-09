#!/usr/bin/env python3
"""Generate counsel's 8 kHz mono s16 WAV assets.

- ivr_prompt_8k.wav: Kokoro line downsampled 24k→8k
- keyboard_clatter_8k.wav: synthesized key clicks + paper shuffle (~8 s loop)
- hold_music_8k.wav: cheesy synthesized hold music (~16 s loop)
"""

import math
import random
import struct
import wave

SP = __import__("os").path.dirname(__import__("os").path.abspath(__file__))
OUT = __import__("os").path.join(SP, "..", "assets")
SR = 8000
random.seed(1974)  # Puddle v. Splash


def write_wav(path, samples):
    clipped = [max(-32767, min(32767, int(s))) for s in samples]
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(struct.pack(f"<{len(clipped)}h", *clipped))
    print(f"wrote {path} ({len(clipped)/SR:.1f}s)")


def downsample_ivr():
    with wave.open(f"{SP}/ivr_24k.wav", "rb") as w:
        assert w.getframerate() == 24000 and w.getnchannels() == 1
        raw = w.readframes(w.getnframes())
    src = struct.unpack(f"<{len(raw)//2}h", raw)
    # boxcar 3:1 — fine for a phone IVR voice
    out = [(src[i] + src[i + 1] + src[i + 2]) / 3 for i in range(0, len(src) - 3, 3)]
    write_wav(f"{OUT}/ivr_prompt_8k.wav", out)


def key_click(n_samples, amp):
    """A key press: sharp noise attack, fast decay, slight body resonance."""
    out = []
    f_body = random.uniform(900, 1500)
    for i in range(n_samples):
        t = i / n_samples
        env = math.exp(-t * 18)
        noise = random.uniform(-1, 1) * 0.7
        body = 0.3 * math.sin(2 * math.pi * f_body * i / SR)
        out.append(amp * env * (noise + body))
    return out


def clatter():
    dur = 8.0
    samples = [0.0] * int(SR * dur)
    # Typing: bursts of 3-9 keys, pauses between bursts.
    t = 0.3
    while t < dur - 0.5:
        for _ in range(random.randint(3, 9)):
            if t >= dur - 0.2:
                break
            click = key_click(random.randint(200, 400), random.uniform(3000, 7000))
            start = int(t * SR)
            for i, s in enumerate(click):
                if start + i < len(samples):
                    samples[start + i] += s
            t += random.uniform(0.06, 0.16)
        t += random.uniform(0.4, 1.1)
    # A couple of paper shuffles: longer, soft, low-pass-ish noise.
    for shuffle_at in (2.1, 5.7):
        n = int(SR * random.uniform(0.5, 0.8))
        start = int(shuffle_at * SR)
        prev = 0.0
        for i in range(n):
            t01 = i / n
            env = math.sin(math.pi * t01)  # swell in and out
            raw = random.uniform(-1, 1)
            prev = prev * 0.82 + raw * 0.18  # crude low-pass
            if start + i < len(samples):
                samples[start + i] += 2200 * env * prev
    write_wav(f"{OUT}/keyboard_clatter_8k.wav", samples)


def hold_music():
    """Public-domain-adjacent elevator noodling: arpeggios over I-vi-IV-V."""
    dur_beat = 0.32
    chords = [
        (261.63, 329.63, 392.00),  # C
        (220.00, 261.63, 329.63),  # Am
        (174.61, 220.00, 261.63),  # F
        (196.00, 246.94, 293.66),  # G
    ]
    samples = []
    for _ in range(2):  # two passes = ~16 s
        for chord in chords:
            seq = list(chord) + [chord[1], chord[2], chord[0] * 2, chord[2], chord[1]]
            for note in seq[:8]:
                n = int(SR * dur_beat)
                for i in range(n):
                    t = i / SR
                    env = min(1.0, i / 200) * math.exp(-t * 2.2)
                    vib = 1 + 0.004 * math.sin(2 * math.pi * 5.5 * t)
                    s = 6500 * env * math.sin(2 * math.pi * note * vib * t)
                    s += 1800 * env * math.sin(2 * math.pi * note * 2 * t)  # cheap brightness
                    # bass root, held
                    s += 2500 * math.sin(2 * math.pi * chord[0] / 2 * t) * 0.6
                    samples.append(s)
    write_wav(f"{OUT}/hold_music_8k.wav", samples)


downsample_ivr()
clatter()
hold_music()
