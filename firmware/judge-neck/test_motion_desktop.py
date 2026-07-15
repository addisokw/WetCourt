"""Desktop test for the judge-neck motion safety layer (CPython, no hardware).

Run: python3 firmware/judge-neck/test_motion_desktop.py

Imports the REAL firmware/judge-neck/main.py against fake `machine`, `time`,
and `wetline` modules (the fake clock only advances when the test says so),
and asserts the property the tilt mount depends on: the commanded pulse never
jumps — not at cold boot (soft-home from droop), not on AIM (slew limit), not
after a stalled loop (dt cap) — plus the warm-reset readback and the
overcurrent watchdog's trip/re-arm behavior.
"""
import struct
import sys
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent

# ---- fake MicroPython environment ----------------------------------------------


class FakeClock:
    def __init__(self):
        self.ms = 1000

    def ticks_ms(self):
        return self.ms

    def ticks_diff(self, a, b):
        return a - b

    def ticks_add(self, a, d):
        return a + d

    def sleep_ms(self, n):
        self.ms += n


class FakeI2C:
    """Records every pulse write; readback + current + failure injectable."""

    def __init__(self, *a, **kw):
        self.writes = []          # (reg, value) for pulse regs, (reg, bytes) else
        self.reads = []           # every register read (bus-silence checks)
        self.held = {}            # reg -> u16 the STM32 "holds" (warm readback)
        self.current = 0.1        # amps returned from REG_CURRENT (0xA0)
        self.fail_writes = False
        self.fail_reads = False
        FakeMachine.last_i2c = self

    def writeto_mem(self, addr, reg, data):
        assert addr == 0x25
        if self.fail_writes:
            raise OSError(19)     # ENODEV-ish: board off the bus
        if 0x60 <= reg < 0x70:
            self.writes.append((reg, int.from_bytes(data, "little")))
        else:
            self.writes.append((reg, bytes(data)))

    def readfrom_mem(self, addr, reg, n):
        assert addr == 0x25
        self.reads.append(reg)
        if self.fail_reads:
            raise OSError(19)
        if reg == 0xA0:
            return struct.pack("<f", self.current)
        if reg in self.held:
            return self.held[reg].to_bytes(n, "little")
        raise OSError(19)

    def pulses(self, ch):
        reg = 0x60 + ch * 2
        return [v for r, v in self.writes if r == reg]


class FakeMachine(types.ModuleType):
    PWRON_RESET = 1
    HARD_RESET = 2
    SOFT_RESET = 5
    last_i2c = None
    cause = 1

    def __init__(self):
        super().__init__("machine")
        self.Pin = lambda *a, **kw: None
        self.I2C = FakeI2C
        self.reset_cause = lambda: FakeMachine.cause


def import_main(cause, held=None, fail_writes=False):
    """Import a fresh main.py under the fakes; returns (main, i2c, wetline)."""
    clock = FakeClock()
    fake_time = types.ModuleType("time")
    for name in ("ticks_ms", "ticks_diff", "ticks_add", "sleep_ms"):
        setattr(fake_time, name, getattr(clock, name))
    fake_wetline = types.ModuleType("wetline")
    fake_wetline.calls = []
    fake_wetline.run = lambda *a, **kw: fake_wetline.calls.append((a, kw))
    FakeMachine.cause = cause
    fakes = {"time": fake_time, "machine": FakeMachine(), "wetline": fake_wetline}
    # Board state must exist the moment main.py constructs its I2C, so seed
    # it from the constructor rather than after import.
    orig_init = FakeI2C.__init__

    def seeded(self, *a, **kw):
        orig_init(self, *a, **kw)
        self.held.update(held or {})
        self.fail_writes = fail_writes

    FakeI2C.__init__ = seeded
    saved = {k: sys.modules.get(k) for k in list(fakes) + ["main"]}
    sys.modules.pop("main", None)
    sys.modules.update(fakes)
    sys.path.insert(0, str(HERE))
    try:
        import main
    finally:
        sys.path.remove(str(HERE))
        FakeI2C.__init__ = orig_init
        for k, v in saved.items():
            if v is not None:
                sys.modules[k] = v
            elif k != "main":
                sys.modules.pop(k, None)
    main._clock = clock       # let tests drive the clock the firmware sees
    return main, FakeMachine.last_i2c, fake_wetline


def run_ticks(main, ms, step=10, send=None):
    """Advance the fake clock `ms` in `step` chunks, ticking like wetline."""
    _, kw = main._wetline_call
    for _ in range(ms // step):
        main._clock.ms += step
        kw["tick"](send)


def hookup(main, wetline):
    (args, kw) = wetline.calls[0]
    assert args[0] == "judge-neck" and kw["tick"], "run() signature changed?"
    main._wetline_call = (args, kw)
    return args[2]["AIM"]


def max_delta(pulses):
    return max(abs(b - a) for a, b in zip(pulses, pulses[1:])) if len(pulses) > 1 else 0


def widx(i2c, reg, pred=lambda v: True):
    """Index (in write order) of the first write to `reg` matching pred."""
    return next(i for i, (r, v) in enumerate(i2c.writes) if r == reg and pred(v))


# ---- tests ----------------------------------------------------------------------


def test_cold_boot_soft_home():
    main, i2c, wl = import_main(cause=FakeMachine.PWRON_RESET)
    tilt = i2c.pulses(1)
    assert tilt[0] == 2167, "cold boot must start the ramp at droop, got %s" % tilt[0]
    assert tilt[-1] == 1500, "home should end at the boot pose, got %s" % tilt[-1]
    assert all(b <= a for a, b in zip(tilt, tilt[1:])), "home ramp not monotonic"
    # HOME_RATE µs/s over 20 ms sleeps -> at most 5 µs per write (+1 rounding)
    assert max_delta(tilt) <= 6, "home step too big: %d µs" % max_delta(tilt)
    assert i2c.pulses(0) == [1500], "pan should be written once at assumed center"
    # each channel: pulse written before its mode-3 enable
    assert widx(i2c, 0x62) < widx(i2c, 0x01), "tilt mode set before pulse"
    assert widx(i2c, 0x60) < widx(i2c, 0x00), "pan mode set before pulse"
    # droop-zone pan lock: pan must not even be POWERED until tilt has climbed
    # into the working range (a pan snap at droop can crash the head)
    tilt_safe_at = widx(i2c, 0x62, lambda v: v <= 1967)
    assert widx(i2c, 0x60) > tilt_safe_at, "pan powered while tilt was deep"


def test_warm_boot_readback():
    main, i2c, wl = import_main(cause=FakeMachine.SOFT_RESET,
                                held={0x00: 3, 0x01: 3, 0x60: 1583, 0x62: 1800})
    assert i2c.pulses(0)[0] == 1583 and i2c.pulses(1)[0] == 1800, \
        "warm boot must resume from the STM32's held pulses"
    assert i2c.pulses(1)[-1] == 1500 and max_delta(i2c.pulses(1)) <= 6


def test_warm_boot_readback_unavailable():
    main, i2c, wl = import_main(cause=FakeMachine.HARD_RESET)  # reads raise
    assert i2c.pulses(1)[0] == 2167, "no readback -> assume droop (safe direction)"


def test_warm_boot_but_stm32_was_power_cycled():
    # Brownout: ESP32 reports a non-PWRON cause but the servo board's
    # registers are back at defaults (mode 0, pulse plausibly 1500) — the
    # mode check must reject the readback and assume droop.
    main, i2c, wl = import_main(cause=FakeMachine.HARD_RESET,
                                held={0x00: 0, 0x01: 0, 0x60: 1500, 0x62: 1500})
    assert i2c.pulses(1)[0] == 2167, "fresh STM32 spoofed the warm readback"


def test_aim_slews_and_clamps():
    main, i2c, wl = import_main(cause=FakeMachine.PWRON_RESET)
    aim = hookup(main, wl)
    i2c.writes.clear()
    assert aim(["1583", "1900"]) is None
    run_ticks(main, 2000)
    assert i2c.pulses(0)[-1] == 1583 and i2c.pulses(1)[-1] == 1900
    # AIM_RATE µs/s over 10 ms ticks -> at most 6 µs per write (+1 rounding)
    assert max_delta(i2c.pulses(1)) <= 7, "AIM step too big"
    assert aim(["100", "9999"]) is None       # host went mad: clamp, don't obey
    run_ticks(main, 4000)
    assert i2c.pulses(1)[-1] == 2500, "tilt should clamp to 2500"
    # ...and that clamped tilt is deep, so pan must freeze mid-travel:
    deep_at = widx(i2c, 0x62, lambda v: v > 1967)
    last_pan = max(i for i, (r, v) in enumerate(i2c.writes) if r == 0x60)
    assert last_pan < deep_at, "pan kept moving after tilt went deep"
    assert i2c.pulses(0)[-1] > 500, "pan should have frozen short of its target"
    assert aim(["1500", "1500"]) is None      # sane again: tilt climbs out,
    run_ticks(main, 4000)                     # then pan unlocks and finishes
    assert i2c.pulses(0)[-1] == 1500 and i2c.pulses(1)[-1] == 1500
    assert aim([]) == "missing_args"
    assert aim(["1500"]) == "need_pan_tilt"
    assert aim(["abc", "def"]) == "bad_args"


def test_stalled_loop_dt_cap():
    main, i2c, wl = import_main(cause=FakeMachine.PWRON_RESET)
    aim = hookup(main, wl)
    assert aim(["1583", "1900"]) is None
    i2c.writes.clear()
    main._clock.ms += 15000                   # e.g. a WiFi reassociate stall
    main._wetline_call[1]["tick"](None)
    tilt = i2c.pulses(1)
    assert tilt and tilt[0] - 1500 <= 60, \
        "capped tick moved %d µs (limit 60)" % (tilt[0] - 1500)


def test_overcurrent_watchdog():
    main, i2c, wl = import_main(cause=FakeMachine.PWRON_RESET)
    aim = hookup(main, wl)
    sent = []
    assert aim(["1583", "1900"]) is None
    run_ticks(main, 300, send=sent.append)    # mid-move...
    i2c.current = 3.0                         # ...a sustained stall develops
    run_ticks(main, 1200, send=sent.append)
    assert sent == ["OVERCURRENT 3.00"], "expected one trip, got %s" % sent
    assert main._tgt[1] == 1967, \
        "trip must ease tilt to TILT_SAFE (full droop crashes at pan != 0)"
    run_ticks(main, 2000, send=sent.append)   # still pegged: no re-trip
    assert len(sent) == 1, "watchdog re-tripped without recovering first"
    i2c.current = 0.1                         # jam cleared: recover + re-arm
    assert aim(["1583", "1700"]) is None      # motion re-opens the poll window
    run_ticks(main, 700, send=sent.append)
    i2c.current = 3.0                         # stalls again during/after move
    run_ticks(main, 1500, send=sent.append)
    assert len(sent) == 2, "watchdog should re-trip after recovery"


def test_no_current_polling_while_parked():
    # 0.4 polled 0xA0 at 4 Hz around the clock; the STM32's software PWM
    # jitters when it services I2C, so the parked head twitched. Parked must
    # now mean a silent bus.
    main, i2c, wl = import_main(cause=FakeMachine.PWRON_RESET)
    aim = hookup(main, wl)
    run_ticks(main, 3000)                     # burn the post-home poll tail
    i2c.reads.clear()
    run_ticks(main, 10000)                    # parked for 10 s
    assert i2c.reads == [], "I2C traffic while parked: %s" % i2c.reads[:5]
    assert aim(["1520", "1520"]) is None      # a move re-opens polling
    run_ticks(main, 500)
    assert 0xA0 in i2c.reads, "no current polling during motion"


def test_i2c_failure_keeps_position():
    main, i2c, wl = import_main(cause=FakeMachine.PWRON_RESET)
    aim = hookup(main, wl)
    i2c.fail_writes = True
    assert aim(["1583", "1900"]) is None      # believed-ok until a write fails
    run_ticks(main, 500)
    assert aim(["1583", "1900"]) == "i2c_fail"
    i2c.fail_writes = False
    i2c.writes.clear()
    run_ticks(main, 3000)
    tilt = i2c.pulses(1)
    assert abs(tilt[0] - 1500) <= 7, "resume jumped: first write %d" % tilt[0]
    assert tilt[-1] == 1900 and max_delta(tilt) <= 7


def test_pan_locked_while_tilt_deep():
    # Board absent at cold boot -> soft-home bails with the head still at
    # droop. Once the board appears, pan must not move (or even be powered)
    # until tilt has climbed into the working range: swinging the drooped
    # head crashes it into the booth when pan is off-center.
    main, i2c, wl = import_main(cause=FakeMachine.PWRON_RESET, fail_writes=True)
    aim = hookup(main, wl)
    i2c.fail_writes = False
    i2c.writes.clear()
    assert aim(["2000", "1600"]) == "i2c_fail"   # board was gone; targets kept
    run_ticks(main, 6000)
    tilt_safe_at = widx(i2c, 0x62, lambda v: v <= 1967)
    assert widx(i2c, 0x60) > tilt_safe_at, "pan moved while tilt was deep"
    assert widx(i2c, 0x00) > tilt_safe_at, "pan powered while tilt was deep"
    assert i2c.pulses(0)[-1] == 2000 and i2c.pulses(1)[-1] == 1600
    assert max_delta(i2c.pulses(0)) <= 7 and max_delta(i2c.pulses(1)) <= 7


if __name__ == "__main__":
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
        print("PASS %s" % t.__name__)
    print("all %d tests passed" % len(tests))
