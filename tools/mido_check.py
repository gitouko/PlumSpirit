#!/usr/bin/env python3
"""Second standard MIDI parser for the export certification (V09-S23,
V09-0099, docs/EXPORT_CERTIFICATION.md step 5).

Reads each Standard MIDI File given on the command line with mido
(https://github.com/mido/mido, MIT licence) -- a TEST TOOL only, never a
PlumSpirit dependency -- and prints its events in the normalised line form
that `crates/plumspirit_engine/tests/common/mido.rs` builds from the
independent Rust reader (`tests/common/smf_digest.rs`):

    file <path>
    header <format> <division> <tracks>
    track <index>
    on <tick> <channel> <key> <velocity>
    off <tick> <channel> <key> <velocity> <ended by Note On 0: 0|1>
    cc <tick> <channel> <controller> <value>
    pb <tick> <channel> <14-bit value>
    pc <tick> <channel> <program>
    press <tick> <channel> <key, or 255 for channel pressure> <value>
    tempo <tick> <microseconds per quarter note>
    ts <tick> <numerator> <denominator power> <clocks> <32nds>
    meta <tick> <type> <payload hex>
    sysex <tick> <payload hex without a leading F0 / trailing F7>
    system <tick> <bytes hex>
    eot <tick>
    error <message>          (mido refused the file)

Within a track each kind is listed in file order, kinds in the order above.
`--unpatched` (first argument) reads with mido exactly as shipped; by
default a mido 1.3.3 bug is worked around (see below).
Exit status 3 when mido is not installed (the Rust test then skips, unless
PLUMSPIRIT_REQUIRE_MIDO=1). Install for a check run only:
`pip install mido==1.3.3`.
"""

import sys

try:
    import mido
    import mido.midifiles.midifiles as _midifiles
    from mido.midifiles.meta import UnknownMetaMessage
except ImportError:
    print("mido is not installed", file=sys.stderr)
    sys.exit(3)


# mido 1.3.3 drops the delta time of a meta event whose type it does not
# know (`build_meta_message` returns `UnknownMetaMessage(meta_type, data)`
# without `time=delta`), so every later event of that track is read too
# early. Found by this certification (V09-S23 step 5, an imported file with
# an `FF 60` event): the tool passes the delta through, as SMF 1.0 requires;
# `--unpatched` reads with mido as shipped.
_build_meta_message = _midifiles.build_meta_message


def _build_meta_message_keeping_time(meta_type, data, delta=0):
    msg = _build_meta_message(meta_type, data, delta)
    if isinstance(msg, UnknownMetaMessage):
        msg.time = delta
    return msg


KINDS = ("on", "off", "cc", "pb", "pc", "press", "tempo", "ts", "meta",
         "sysex", "system", "eot")


def meta_raw(msg):
    """(type, payload) of a meta message as mido encodes it."""
    b = msg.bytes()
    i = 2
    length = 0
    while True:
        c = b[i]
        i += 1
        length = (length << 7) | (c & 0x7F)
        if c < 0x80:
            break
    return b[1], bytes(b[i:i + length])


def track_lines(track):
    out = {k: [] for k in KINDS}
    tick = 0
    for msg in track:
        tick += msg.time
        t = msg.type
        if t == "note_on" and msg.velocity > 0:
            out["on"].append(f"on {tick} {msg.channel} {msg.note} {msg.velocity}")
        elif t == "note_on":
            out["off"].append(f"off {tick} {msg.channel} {msg.note} 0 1")
        elif t == "note_off":
            out["off"].append(
                f"off {tick} {msg.channel} {msg.note} {msg.velocity} 0")
        elif t == "control_change":
            out["cc"].append(
                f"cc {tick} {msg.channel} {msg.control} {msg.value}")
        elif t == "pitchwheel":
            out["pb"].append(f"pb {tick} {msg.channel} {msg.pitch + 8192}")
        elif t == "program_change":
            out["pc"].append(f"pc {tick} {msg.channel} {msg.program}")
        elif t == "aftertouch":
            out["press"].append(f"press {tick} {msg.channel} 255 {msg.value}")
        elif t == "polytouch":
            out["press"].append(
                f"press {tick} {msg.channel} {msg.note} {msg.value}")
        elif t == "sysex":
            out["sysex"].append(f"sysex {tick} {bytes(msg.data).hex()}")
        elif msg.is_meta:
            ty, payload = meta_raw(msg)
            if ty == 0x2F:
                out["eot"].append(f"eot {tick}")
            elif ty == 0x51 and len(payload) == 3:
                us = int.from_bytes(payload, "big")
                out["tempo"].append(f"tempo {tick} {us}")
            elif ty == 0x58 and len(payload) == 4:
                n, d, c, b = payload
                out["ts"].append(f"ts {tick} {n} {d} {c} {b}")
            else:
                out["meta"].append(f"meta {tick} {ty} {payload.hex()}")
        else:
            out["system"].append(f"system {tick} {bytes(msg.bytes()).hex()}")
    lines = []
    for k in KINDS:
        lines.extend(out[k])
    return lines


def main(paths):
    for path in paths:
        print(f"file {path}")
        try:
            f = mido.MidiFile(path)
        except Exception as e:  # mido refuses: report, never crash
            msg = " ".join(str(e).split()) or type(e).__name__
            print(f"error {type(e).__name__}: {msg}")
            continue
        print(f"header {f.type} {f.ticks_per_beat & 0xFFFF} {len(f.tracks)}")
        for i, track in enumerate(f.tracks):
            print(f"track {i}")
            for line in track_lines(track):
                print(line)


if __name__ == "__main__":
    args = sys.argv[1:]
    if args[:1] == ["--unpatched"]:
        args = args[1:]
    else:
        _midifiles.build_meta_message = _build_meta_message_keeping_time
    main(args)
