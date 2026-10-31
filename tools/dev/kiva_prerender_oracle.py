#!/usr/bin/env python3
"""L-C02: predict whether Kiva v1.1.18-1m's pre-render audio engine gets any event to play.

PlumSpirit's own reading of a Standard MIDI File; nothing here is Kiva code. The rule it
applies was taken from reading the source of the owner's Kiva build (MrCHB1/Kiva-Modded, tag
v1.1.18-1m, 14b67d4, 2022-12-30) — see docs/references/v1_0_launch/KIVA_NOTES.md:

* audio note events = Note Ons with velocity >= the Audio Note Threshold (velocity 0 is a
  release), the Note Offs that end those notes, and polyphonic key pressure;
* control events = controller, program change, channel pressure and pitch bend messages
  (tempo / meta / SysEx are not audio events);
* with the default "Pre-render" audio engine, a file whose audio note events and control
  events are BOTH zero hands the event merger an empty list, which never returns: Kiva freezes
  right after "Analyzing MIDI · Tracks N of N" (the realtime KDMAPI / WinMM engines skip
  empty sets and are not affected).

It also prints what Piano From Above sends for these notes (L-C02, PFA loudness): its
playback multiplies every Note On velocity by
min(volume * 127 * notes / velocity_sum, 127 / loudest_velocity) (read in the PFA source,
github.com/kaydax/bpfa GameState.cpp, the same formula in the splash and the original main
screen; volume = PFA's volume slider, 1.0 at full). At full volume that is 127 / loudest, so a
file whose loudest note is velocity 1 plays every note at 127.

usage: kiva_prerender_oracle.py [--threshold N] FILE.mid ...
"""
import sys


def vlq(b, i):
    v = 0
    while True:
        c = b[i]
        i += 1
        v = (v << 7) | (c & 0x7F)
        if c < 0x80:
            return v, i


def count(path, threshold):
    b = open(path, "rb").read()
    if b[:4] != b"MThd":
        raise ValueError("not a Standard MIDI File")
    hl = int.from_bytes(b[4:8], "big")
    i = 8 + hl
    notes = audio = control = poly = 0
    loudest = vel_sum = 0
    while i + 8 <= len(b):
        tag, ln = b[i:i + 4], int.from_bytes(b[i + 4:i + 8], "big")
        i += 8
        end = min(i + ln, len(b))
        if tag != b"MTrk":
            i = end
            continue
        held = {}  # (channel, key) -> stack of velocities of sounding notes
        run = 0
        j = i
        while j < end:
            _, j = vlq(b, j)
            st = b[j]
            if st < 0x80:
                st = run
            else:
                j += 1
            if st == 0xFF:
                j += 1
                ln2, j = vlq(b, j)
                j += ln2
                continue
            if st in (0xF0, 0xF7):
                ln2, j = vlq(b, j)
                j += ln2
                continue
            run = st
            kind, ch = st & 0xF0, st & 0x0F
            if kind in (0x80, 0x90, 0xA0, 0xB0, 0xE0):
                d1, d2 = b[j], b[j + 1]
                j += 2
            else:
                d1, d2 = b[j], 0
                j += 1
            if kind == 0x90 and d2 != 0:
                notes += 1
                loudest = max(loudest, d2)
                vel_sum += d2
                held.setdefault((ch, d1), []).append(d2)
                if d2 >= threshold:
                    audio += 1
            elif kind == 0x80 or (kind == 0x90 and d2 == 0):
                stack = held.get((ch, d1))
                if stack and stack.pop() >= threshold:
                    audio += 1
            elif kind == 0xA0:
                poly += 1
                audio += 1
            else:
                control += 1
        i = end
    return notes, audio, control, poly, loudest, vel_sum


def pfa_factor(notes, loudest, vel_sum, volume=1.0):
    """Piano From Above's velocity multiplier (1.0 for a file without notes)."""
    max_correct = 127.0 / loudest if loudest > 0 else 1.0
    vol_correct = volume * 127.0 * notes / vel_sum if vel_sum > 0 else 1.0
    return min(vol_correct, max_correct)


def main(argv):
    threshold = 15
    if argv[:1] == ["--threshold"]:
        threshold = int(argv[1])
        argv = argv[2:]
    for p in argv:
        notes, audio, control, poly, loudest, vel_sum = count(p, threshold)
        verdict = "loads" if audio or control else "FREEZES (pre-render engine: no event at all)"
        f = pfa_factor(notes, loudest, vel_sum)
        print(f"{p}\tnotes {notes}\taudio note events {audio}\tcontrol events {control}"
              f"\tpoly pressure {poly}\tthreshold {threshold}\t{verdict}"
              f"\tPFA: loudest {loudest}, x{f:.2f} (velocity 1 -> {min(127, int(f + 0.5))},"
              f" {loudest} -> {min(127, int(loudest * f + 0.5))})")


if __name__ == "__main__":
    main(sys.argv[1:])
