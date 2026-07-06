# Wet Court judge-face — the 5 judge personas: color ramp + motion character.
#
# Canonical values come from the web prototype's PERSONAS table (see
# docs/wet-court-eye-face-brief.md §3). Each persona is a set of gradient
# stops, linearly interpolated; named tones are samples of that ramp.
# `speed` scales the eye's animation clock; `fold` is unused by the Eye.

ORDER = ("honorable", "magistrate", "cosmic", "nullpointer", "petunia")

PERSONAS = {
    "honorable": {
        "name": "The Honorable",
        "speed": 0.55,
        "stops": ((0.00, (6, 4, 8)), (0.25, (40, 8, 12)), (0.50, (110, 20, 22)),
                  (0.72, (190, 120, 40)), (0.90, (214, 170, 80)), (1.00, (245, 228, 170))),
    },
    "magistrate": {
        "name": "MAGISTRATE.exe",
        "speed": 1.25,
        "stops": ((0.00, (2, 6, 4)), (0.30, (4, 40, 20)), (0.55, (10, 120, 50)),
                  (0.78, (40, 220, 90)), (1.00, (170, 255, 185))),
    },
    "cosmic": {
        "name": "Cosmic Arbiter",
        "speed": 0.40,
        "stops": ((0.00, (4, 4, 14)), (0.28, (20, 12, 60)), (0.50, (60, 20, 110)),
                  (0.72, (40, 110, 170)), (0.90, (80, 200, 220)), (1.00, (200, 240, 255))),
    },
    "nullpointer": {
        "name": "NULLPOINTER",
        "speed": 1.60,
        "stops": ((0.00, (10, 2, 16)), (0.30, (120, 0, 90)), (0.50, (255, 0, 170)),
                  (0.70, (120, 40, 200)), (0.85, (0, 220, 255)), (1.00, (235, 255, 255))),
    },
    "petunia": {
        "name": "Justice Petunia",
        "speed": 0.70,
        "stops": ((0.00, (12, 6, 2)), (0.30, (60, 28, 4)), (0.55, (160, 86, 8)),
                  (0.78, (235, 150, 40)), (0.92, (255, 190, 90)), (1.00, (255, 238, 200))),
    },
}

# Named tone sample positions on the ramp (prototype convention).
_TONES = (("bg", 0.00), ("dim", 0.16), ("primary", 0.55),
          ("secondary", 0.80), ("accent", 0.98))


def ramp(stops, pos):
    """Sample a stop list at pos in [0,1] (linear interpolation)."""
    if pos <= stops[0][0]:
        return tuple(stops[0][1])
    for i in range(1, len(stops)):
        p1, c1 = stops[i]
        if pos <= p1:
            p0, c0 = stops[i - 1]
            m = (pos - p0) / (p1 - p0)
            return tuple(int(c0[j] + (c1[j] - c0[j]) * m + 0.5) for j in range(3))
    return tuple(stops[-1][1])


def tones(slug):
    """Named tones (bg/dim/primary/secondary/accent) for a persona slug."""
    stops = PERSONAS[slug]["stops"]
    return {name: ramp(stops, pos) for name, pos in _TONES}
