# Wet Court judge-face — EyeFace: the animated eye, rendered with layered
# displayio so per-frame Python work stays tiny (brief §4a).
#
# Layers (bottom → top):
#   0  background   full-panel fill; pure black in normal phases (LEDs off
#                   beyond the eye), flooded by the verdict recolor
#   1  iris tile    one small bitmap: sclera halo + striated iris + limbal
#                   ring + pupil + catchlight. Gaze = move the TileGrid.
#                   Dilation = redraw only the ~19x19 pupil box.
#
# No eyelids/blink: the prototype's lid bars read as the frame shrinking on
# the physical portrait panel, so they're removed (operator preference).
#
# Verdict effects don't touch the bitmaps at all: guilty strobes by lerping
# every palette entry toward red at ~10 Hz plus a whole-face horizontal
# jitter (stands in for the prototype's per-row glitch shifts); innocent
# blooms green via the same palette lerp, easing out over ~2 s. displayio
# has no alpha blending, so palette recolor is the cheap faithful substitute
# for the prototype's full-frame overlays.
#
# Geometry is parameterized from the display (works portrait or landscape).
# Deviation from the prototype: iris striations are static per persona (the
# M4 can't re-texture the disc every frame); drift + dilation carry the
# "alive" reading.

import math
import random

import displayio

import personas

PHASES = ("idle", "listening", "deliberating", "verdict:guilty", "verdict:innocent")

# Iris-tile palette layout.
_P_PUPIL = 1                      # near-black pupil
_P_CATCH = 2                      # white catchlight
_P_HALO0 = 3                      # 8 sclera-halo shades: 3..10
_P_IRIS0 = 11                     # 12 mix steps x 4 brightness: 11..58
_P_COUNT = 64                     # bitmap value_count (>=59, power of two)
_BRIGHT = (0.30, 0.55, 0.80, 1.00)  # limbal-ring brightness quantization

_BG = (0, 0, 0)                   # off pixels stay truly off (was (8,6,8))
_RED = (255, 36, 18)              # guilty strobe target
_GREEN = (46, 220, 96)            # innocent bloom target

# Catchlight: compact ~1.3 px specular blob at (-3,-3) from the eye center.
_CATCH_OFF = ((0, 0), (1, 0), (0, 1), (1, 1))


def snoise(t, seed=0.0):
    """Smooth wandering pseudo-noise in [0,1] (stand-in for Perlin noise)."""
    return 0.5 + 0.5 * (0.62 * math.sin(t * 1.7 + seed)
                        + 0.38 * math.sin(t * 2.93 + seed * 1.7 + 1.0))


def _mix(c0, c1, m):
    return (int(c0[0] + (c1[0] - c0[0]) * m),
            int(c0[1] + (c1[1] - c0[1]) * m),
            int(c0[2] + (c1[2] - c0[2]) * m))


def _scale(c, s):
    return (int(c[0] * s), int(c[1] * s), int(c[2] * s))


def _rgb(c):
    return (c[0] << 16) | (c[1] << 8) | c[2]


class EyeFace:
    def __init__(self, display, persona="honorable"):
        self.W = display.width
        self.H = display.height
        self.CX = self.W / 2
        self.CY = self.H / 2
        # Sized so iris + halo fit *inside* the narrow axis: the brief's 0.38
        # ratio + 6 px halo overhung a 32 px-wide portrait panel and clipped
        # flat at the edges. 0.34 + 4 px → 31 px tile on a 32 px panel.
        self.IR = min(self.W, self.H) * 0.34     # iris radius (~10.9 on 32px)
        self.HALO = 4.0                          # sclera glow falloff (px)
        self.TR = int(self.IR + self.HALO) + 1   # tile "radius"
        self.TILE = 2 * self.TR + 1              # iris tile is TILE x TILE

        # -- layers --------------------------------------------------------
        self._bg_pal = displayio.Palette(1)
        self._bg_pal[0] = _rgb(_BG)
        bg_bmp = displayio.Bitmap(self.W, self.H, 1)
        bg = displayio.TileGrid(bg_bmp, pixel_shader=self._bg_pal)

        self._iris_pal = displayio.Palette(_P_COUNT)
        self._iris_pal.make_transparent(0)
        self._iris_bmp = displayio.Bitmap(self.TILE, self.TILE, _P_COUNT)
        self._iris = displayio.TileGrid(self._iris_bmp, pixel_shader=self._iris_pal)

        self.group = displayio.Group()
        self.group.append(bg)
        self.group.append(self._iris)

        # -- state ---------------------------------------------------------
        self._t = 0.0                 # persona-speed-scaled animation clock
        self._phase = "idle"
        self._phase_elapsed = 0.0     # real seconds since last phase change
        self._a_target = 0.0          # raw audio level from the host
        self._a = 0.0                 # smoothed
        self._dil = -1                # current pupil radius (px, quantized)
        self._blend = None            # (target_rgb, f) currently on the palettes
        self._slug = None
        self._cols = None             # base palette colors for active persona
        self._iris_base = None        # iris indices w/o pupil, for restores
        self._cache = {}              # slug -> (base bytearray, colors list)

        self.set_persona(persona)
        display.root_group = self.group

    # ----------------------------------------------------------------- API
    def set_persona(self, name):
        slug = str(name).strip().lower()
        if slug not in personas.PERSONAS:
            raise ValueError("unknown persona")
        if slug == self._slug:
            return
        cached = self._cache.get(slug)
        if cached is None:
            cached = self._build_iris(slug)
            self._cache[slug] = cached
        self._iris_base, self._cols = cached
        self._slug = slug
        self._speed = personas.PERSONAS[slug]["speed"]
        self._seed = 0
        for ch in slug:
            self._seed = (self._seed * 31 + ord(ch)) % 997

        self._blit_base()
        self._dil = -1                # force pupil redraw on next tick
        self._blend = None            # force palette (re)apply
        self._apply_blend((0, 0, 0), 0.0)

    def set_phase(self, phase):
        if phase not in PHASES:
            raise ValueError("unknown phase")
        if phase == self._phase:
            return
        self._phase = phase
        self._phase_elapsed = 0.0
        if not phase.startswith("verdict"):
            self._apply_blend((0, 0, 0), 0.0)
            self.group.x = 0

    def set_audio(self, level):
        self._a_target = min(1.0, max(0.0, level))

    def tick(self, dt):
        """Advance drift/dilation and update the layers. Call every frame."""
        phase = self._phase
        deliberating = phase == "deliberating"
        self._t += dt * self._speed * (1.6 if deliberating else 1.0)
        self._phase_elapsed += dt
        t = self._t

        # Audio envelope (only drives the pupil while listening). Snap when
        # close so the asymptote actually reaches the dilation extremes.
        self._a += (self._a_target - self._a) * min(1.0, dt * 10)
        if abs(self._a_target - self._a) < 0.004:
            self._a = self._a_target
        a = self._a if phase == "listening" else 0.0

        # 1. Gaze drift (brief §3.1) — vertical only: the panel is mounted on
        #    the judge-neck pan/tilt mech, which owns horizontal gaze, and the
        #    portrait width has no room to wander sideways anyway. Listening
        #    darts a little more; deliberating adds a fast jitter term.
        amp = 1.3 if phase == "listening" else 1.0
        ly = (snoise(t * 0.25, self._seed * 0.29 + 4.2) - 0.5) * (self.H * 0.11) * amp
        if deliberating:
            ly += (snoise(t * 1.7, 5.1) - 0.5) * 2.5
        gx = int(self.CX + 0.5) - self.TR
        gy = int(self.CY + ly + 0.5) - self.TR
        if gx != self._iris.x:
            self._iris.x = gx
        if gy != self._iris.y:
            self._iris.y = gy

        # 2. Pupil dilation (brief §3.2): 3 + a*4.5 px, quantized to 1 px.
        dil = min(8, max(3, int(3.0 + a * 4.5 + 0.5)))
        if dil != self._dil:
            self._dil = dil
            self._redraw_pupil()

        # 3. Verdict overrides (palette recolor + jitter). (No blink/lids —
        #    see header note.)
        if phase == "verdict:guilty":
            on = (self._phase_elapsed * 10.0) % 1.0 < 0.5   # ~10 Hz strobe
            self._apply_blend(_RED, 0.85 if on else 0.0)
            self.group.x = random.randint(-2, 2) if on else 0
        elif phase == "verdict:innocent":
            f = max(0.0, 1.0 - self._phase_elapsed / 2.0) * 0.75
            self._apply_blend(_GREEN, f)

    # ------------------------------------------------------------ internals
    def _build_iris(self, slug):
        """Render one persona's iris tile → (index bytearray, palette colors)."""
        tone = personas.tones(slug)
        seed = sum(ord(c) for c in slug) * 0.618

        cols = [(0, 0, 0)] * _P_COUNT
        cols[_P_PUPIL] = (4, 3, 5)
        cols[_P_CATCH] = (255, 255, 255)
        for i in range(8):            # halo: dim tone fading into background
            cols[_P_HALO0 + i] = _mix(tone["dim"], _BG, i / 7.0)
        for mq in range(12):          # iris: primary→secondary mix x brightness
            base = _mix(tone["primary"], tone["secondary"], 0.35 + ((mq + 0.5) / 12.0) * 0.5)
            for bq in range(4):
                cols[_P_IRIS0 + mq * 4 + bq] = _scale(base, _BRIGHT[bq])

        TR, TILE, IR, HALO = self.TR, self.TILE, self.IR, self.HALO
        base = bytearray(TILE * TILE)
        i = 0
        for y in range(TILE):
            dy = y - TR
            for x in range(TILE):
                dx = x - TR
                r = math.sqrt(dx * dx + dy * dy)
                if r <= IR:
                    k = r / IR
                    ang = math.atan2(dy, dx)
                    # Radial striations (static texture; see header note).
                    stri = 0.5 + 0.5 * math.sin(ang * 9.0 + seed
                                                + 2.2 * math.sin(ang * 3.0 - seed)
                                                + k * 3.0)
                    m = 0.35 + stri * 0.5 * (1.0 - k)
                    mq = min(11, int((m - 0.35) * 2.0 * 12.0))
                    limb = 1.0 if k <= 0.82 else max(0.0, 1.0 - (k - 0.82) / 0.18)
                    base[i] = _P_IRIS0 + mq * 4 + min(3, int(limb * 4.0))
                elif r <= IR + HALO:
                    s = 1.0 - (r - IR) / HALO
                    base[i] = _P_HALO0 + min(7, int((1.0 - s) * 8.0))
                # else: 0 = transparent
                i += 1
        return base, cols

    def _blit_base(self):
        """Copy the persona's iris indices into the live bitmap."""
        bmp, base, TILE = self._iris_bmp, self._iris_base, self.TILE
        try:
            import bitmaptools
            bitmaptools.arrayblit(bmp, base)
        except (ImportError, AttributeError, ValueError, TypeError):
            i = 0
            for y in range(TILE):
                for x in range(TILE):
                    bmp[x, y] = base[i]
                    i += 1

    def _redraw_pupil(self):
        """Rewrite only the pupil bounding box + catchlight for the current dil."""
        bmp, base = self._iris_bmp, self._iris_base
        TR, TILE = self.TR, self.TILE
        # d² - d + 1 keeps the tiny disc round (plain d² reads as a square).
        d2 = self._dil * self._dil - self._dil + 1
        R = 9                          # covers max dilation (8) + restore ring
        for y in range(TR - R, TR + R + 1):
            dy = y - TR
            row = y * TILE
            for x in range(TR - R, TR + R + 1):
                dx = x - TR
                v = _P_PUPIL if dx * dx + dy * dy < d2 else base[row + x]
                if bmp[x, y] != v:
                    bmp[x, y] = v
        for ox, oy in _CATCH_OFF:      # catchlight rides the pupil's upper-left
            bmp[TR - 3 + ox, TR - 3 + oy] = _P_CATCH

    def _apply_blend(self, target, f):
        """Lerp every palette toward `target` by f (f=0 restores persona colors)."""
        if self._blend == (target, f):
            return
        self._blend = (target, f)
        pal, cols = self._iris_pal, self._cols
        if f <= 0.0:
            for i in range(1, _P_COUNT):
                pal[i] = _rgb(cols[i])
            self._bg_pal[0] = _rgb(_BG)
        else:
            for i in range(1, _P_COUNT):
                pal[i] = _rgb(_mix(cols[i], target, f))
            self._bg_pal[0] = _rgb(_mix(_BG, target, f * 0.6))
