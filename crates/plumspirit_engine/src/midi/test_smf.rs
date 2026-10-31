//! Synthetic Standard MIDI Files for the worker-equivalence tests
//! (v1.0-beta1 B1-0161 / B1-0162).

pub(crate) fn vlq(mut v: u64) -> Vec<u8> {
    let mut out = vec![(v & 0x7F) as u8];
    v >>= 7;
    while v > 0 {
        out.push(0x80 | (v & 0x7F) as u8);
        v >>= 7;
    }
    out.reverse();
    out
}

/// A chunk of (delta, raw bytes) events, written as given.
pub(crate) fn chunk(kind: &[u8; 4], events: &[(u64, &[u8])]) -> Vec<u8> {
    let mut data = Vec::new();
    for (d, e) in events {
        data.extend(vlq(*d));
        data.extend_from_slice(e);
    }
    let mut c = kind.to_vec();
    c.extend((data.len() as u32).to_be_bytes());
    c.extend(data);
    c
}

pub(crate) fn smf(declared: u16, chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut f = b"MThd".to_vec();
    f.extend(6u32.to_be_bytes());
    f.extend(1u16.to_be_bytes());
    f.extend(declared.to_be_bytes());
    f.extend(480u16.to_be_bytes());
    for c in chunks {
        f.extend_from_slice(c);
    }
    f
}

pub(crate) const EOT: &[u8] = &[0xFF, 0x2F, 0x00];

/// Tracks with tempo / meta / SysEx, running status, same-key overlaps,
/// unmatched Note On / Off, controllers, running status across a meta, a
/// system message, data after End of Track, a track without End of Track, an
/// unknown chunk between tracks and a wrong declared count.
pub(crate) fn rich() -> Vec<u8> {
    let conductor = chunk(
        b"MTrk",
        &[
            (0, &[0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]),
            (0, &[0xFF, 0x58, 0x04, 0x04, 0x02, 0x18, 0x08]),
            (0, &[0xFF, 0x03, 0x04, b'S', b'o', b'n', b'g']),
            (960, &[0xFF, 0x51, 0x03, 0x06, 0x1A, 0x80]),
            (0, EOT),
        ],
    );
    let notes = chunk(
        b"MTrk",
        &[
            (0, &[0x90, 60, 100]),
            (0, &[64, 90]),
            (10, &[60, 80]),
            (10, &[0x80, 60, 5]),
            (0, &[60, 7]),
            (5, &[0x90, 64, 0]),
            (0, &[0xB0, 7, 100]),
            (0, &[0xF0, 0x03, 0x7E, 0x7F, 0xF7]),
            (3, &[0x81, 61, 0]),
            (1, &[0x91, 62, 50]),
            (0, EOT),
        ],
    );
    let lenient = chunk(
        b"MTrk",
        &[
            (0, &[0x92, 70, 60]),
            (0, &[0xFF, 0x01, 0x01, b'x']),
            (4, &[70, 0]),
            (0, &[0xF8]),
            (2, &[0xC2, 5]),
            (0, EOT),
            (0, &[0x90, 1, 1]),
        ],
    );
    let unknown = chunk(b"XFIH", &[(0, &[1, 2, 3])]);
    let no_eot = chunk(b"MTrk", &[(0, &[0x93, 72, 40]), (8, &[0x83, 72, 0])]);
    let wide = chunk(
        b"MTrk",
        &[(0, &[0x94, 0x7F, 1]), (1, &[0x84, 0x7F, 0]), (0, EOT)],
    );
    smf(9, &[conductor, notes, lenient, unknown, no_eot, wide])
}

/// A deterministic stream of mutated copies of `base` (byte changes, high
/// bits flipped, truncations, insertions).
pub(crate) fn mutations(base: &[u8], count: usize) -> Vec<Vec<u8>> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..count)
        .map(|_| {
            let mut f = base.to_vec();
            for _ in 0..1 + next() % 3 {
                let i = (next() as usize) % f.len();
                match next() % 4 {
                    0 => f[i] = next() as u8,
                    1 => f[i] ^= 0x80,
                    2 => f.truncate(i.max(14)),
                    _ => f.insert(i, next() as u8),
                }
            }
            f
        })
        .collect()
}
