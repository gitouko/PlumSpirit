//! Variable-length quantities as used by Standard MIDI Files (7 bits per byte,
//! most significant group first, continuation bit 0x80). SMF limits them to four
//! bytes, i.e. values up to 0x0FFF_FFFF.

use crate::model::timing::MAX_DELTA_TICKS;

/// Appends the VLQ encoding of `value`. Values above `0x0FFF_FFFF` are encoded with
/// more than four groups, which is not valid SMF; callers must validate first
/// (the exporter does, and reports an error instead of writing such values).
pub fn write_vlq(value: u64, out: &mut Vec<u8>) {
    debug_assert!(value <= MAX_DELTA_TICKS, "VLQ value exceeds SMF limit");
    let mut groups = [0u8; 10];
    let mut n = 0;
    let mut v = value;
    loop {
        groups[n] = (v & 0x7F) as u8;
        n += 1;
        v >>= 7;
        if v == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        let cont = if i == 0 { 0 } else { 0x80 };
        out.push(groups[i] | cont);
    }
}

/// Encoded length in bytes.
pub fn vlq_len(value: u64) -> usize {
    let mut n = 1;
    let mut v = value >> 7;
    while v != 0 {
        n += 1;
        v >>= 7;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_examples() {
        // Examples from the SMF 1.0 specification.
        let cases: &[(u64, &[u8])] = &[
            (0x00, &[0x00]),
            (0x40, &[0x40]),
            (0x7F, &[0x7F]),
            (0x80, &[0x81, 0x00]),
            (0x2000, &[0xC0, 0x00]),
            (0x3FFF, &[0xFF, 0x7F]),
            (0x4000, &[0x81, 0x80, 0x00]),
            (0x10_0000, &[0xC0, 0x80, 0x00]),
            (0x1F_FFFF, &[0xFF, 0xFF, 0x7F]),
            (0x20_0000, &[0x81, 0x80, 0x80, 0x00]),
            (0x800_0000, &[0xC0, 0x80, 0x80, 0x00]),
            (0xFFF_FFFF, &[0xFF, 0xFF, 0xFF, 0x7F]),
        ];
        for (v, bytes) in cases {
            let mut out = Vec::new();
            write_vlq(*v, &mut out);
            assert_eq!(&out[..], *bytes, "value {v:#x}");
            assert_eq!(vlq_len(*v), bytes.len());
        }
    }
}
