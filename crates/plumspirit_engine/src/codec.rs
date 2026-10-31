//! Small binary encoding helpers shared by the undo history and the native `.lup`
//! project format: unsigned LEB128 variable-length integers and CRC-32.
//!
//! Every decoder is bounds-checked and returns `None` on truncated or over-long
//! input instead of panicking, because both callers treat their input as untrusted
//! (a `.lup` file may be damaged; history data is internal but verified anyway).

/// Appends `v` as unsigned LEB128 (7 bits per byte, low groups first).
#[inline]
pub fn put_uvarint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Number of bytes [`put_uvarint`] writes for `v`.
#[inline]
pub fn uvarint_len(v: u64) -> usize {
    let bits = 64 - (v | 1).leading_zeros() as usize;
    bits.div_ceil(7)
}

/// Reads an unsigned LEB128 value starting at `*pos`, advancing `pos`. Rejects
/// encodings longer than 10 bytes or values that overflow `u64`.
#[inline]
pub fn get_uvarint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v: u64 = 0;
    let mut shift = 0u32;
    loop {
        let b = *data.get(*pos)?;
        *pos += 1;
        let low = u64::from(b & 0x7F);
        if shift == 63 && low > 1 {
            return None;
        }
        v |= low << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// Appends a signed value as zig-zag LEB128.
#[inline]
pub fn put_ivarint(out: &mut Vec<u8>, v: i64) {
    put_uvarint(out, ((v << 1) ^ (v >> 63)) as u64);
}

#[inline]
pub fn get_ivarint(data: &[u8], pos: &mut usize) -> Option<i64> {
    let u = get_uvarint(data, pos)?;
    Some(((u >> 1) as i64) ^ -((u & 1) as i64))
}

/// CRC-32 (IEEE 802.3, reflected, polynomial 0xEDB88320), as used by zlib/PNG.
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

impl Default for Crc32 {
    fn default() -> Self {
        Crc32::new()
    }
}

impl Crc32 {
    pub const fn new() -> Self {
        Crc32(0xFFFF_FFFF)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        let mut c = self.0;
        for &b in bytes {
            c = CRC_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }

    pub fn finish(&self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }

    pub fn of(bytes: &[u8]) -> u32 {
        let mut c = Crc32::new();
        c.update(bytes);
        c.finish()
    }
}

// ---------------------------------------------------------------- base64

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn b64encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        let k = [18, 12, 6, 0];
        for (i, s) in k.iter().enumerate() {
            if i <= c.len() {
                out.push(B64[((n >> s) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Python's `base64.b64decode(text)` (non-strict `binascii.a2b_base64`):
/// characters outside the alphabet are skipped, a complete pad sequence ends
/// the data, a dangling quad is an error. None: not ASCII / bad padding.
pub fn b64decode_py(text: &str) -> Option<Vec<u8>> {
    if !text.is_ascii() {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut quad, mut left, mut pads) = (0u8, 0u32, 0u32);
    for &c in text.as_bytes() {
        if c == b'=' {
            if quad >= 2 {
                pads += 1;
                if u32::from(quad) + pads >= 4 {
                    return Some(out);
                }
            }
            continue;
        }
        let Some(v) = B64.iter().position(|&b| b == c) else {
            continue;
        };
        let v = v as u32;
        pads = 0;
        match quad {
            0 => {
                quad = 1;
                left = v;
            }
            1 => {
                quad = 2;
                out.push(((left << 2) | (v >> 4)) as u8);
                left = v & 0x0f;
            }
            2 => {
                quad = 3;
                out.push((((left << 4) | (v >> 2)) & 0xff) as u8);
                left = v & 0x03;
            }
            _ => {
                quad = 0;
                out.push((((left << 6) | v) & 0xff) as u8);
                left = 0;
            }
        }
    }
    (quad == 0).then_some(out)
}

// ---------------------------------------------------------------- zlib

/// Adler-32 (zlib's checksum).
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for c in data.chunks(5552) {
        for &x in c {
            a += u32::from(x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    nbits: u32,
}

impl Bits<'_> {
    fn need(&mut self, n: u32) -> Option<u32> {
        while self.nbits < n {
            let b = *self.data.get(self.pos)?;
            self.pos += 1;
            self.bit |= u32::from(b) << self.nbits;
            self.nbits += 8;
        }
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Some(v)
    }
}

/// A canonical Huffman decoding table (puff-style counts / symbols).
struct Huff {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huff {
    fn new(lengths: &[u8]) -> Option<Huff> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[usize::from(l)] += 1;
        }
        let mut left: i32 = 1;
        for &c in &count[1..] {
            left = (left << 1) - i32::from(c);
            if left < 0 {
                return None; // over-subscribed
            }
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[usize::from(offs[usize::from(l)])] = s as u16;
                offs[usize::from(l)] += 1;
            }
        }
        Some(Huff { count, symbol })
    }

    fn decode(&self, b: &mut Bits) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.need(1)? as i32;
            let count = i32::from(self.count[len]);
            if code - count < first {
                return self.symbol.get((index + (code - first)) as usize).copied();
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        None
    }
}

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn fixed_tables() -> (Huff, Huff) {
    let mut l = [0u8; 288];
    l[..144].fill(8);
    l[144..256].fill(9);
    l[256..280].fill(7);
    l[280..].fill(8);
    (
        Huff::new(&l).expect("fixed"),
        Huff::new(&[5u8; 30]).expect("fixed"),
    )
}

/// zlib's `decompress` (a zlib stream; trailing data ignored). None: not a
/// valid stream, or the data would pass `limit` bytes.
pub fn zlib_decompress(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    if data.len() < 2 {
        return None;
    }
    let (cmf, flg) = (data[0], data[1]);
    if cmf & 0x0f != 8
        || cmf >> 4 > 7
        || (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0
        || flg & 0x20 != 0
    {
        return None;
    }
    let mut b = Bits {
        data,
        pos: 2,
        bit: 0,
        nbits: 0,
    };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = b.need(1)?;
        match b.need(2)? {
            0 => {
                b.bit = 0;
                b.nbits = 0;
                let p = b.pos;
                let h = data.get(p..p + 4)?;
                let len = u16::from_le_bytes([h[0], h[1]]);
                if !len != u16::from_le_bytes([h[2], h[3]]) {
                    return None;
                }
                let s = data.get(p + 4..p + 4 + usize::from(len))?;
                if out.len() + s.len() > limit {
                    return None;
                }
                out.extend_from_slice(s);
                b.pos = p + 4 + usize::from(len);
            }
            1 => {
                let (lt, dt) = fixed_tables();
                inflate_block(&mut b, &mut out, &lt, &dt, limit)?;
            }
            2 => {
                let nlen = b.need(5)? as usize + 257;
                let ndist = b.need(5)? as usize + 1;
                let ncode = b.need(4)? as usize + 4;
                if nlen > 286 || ndist > 30 {
                    return None;
                }
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut cl = [0u8; 19];
                for &o in &ORDER[..ncode] {
                    cl[o] = b.need(3)? as u8;
                }
                let ct = Huff::new(&cl)?;
                let mut lens = vec![0u8; nlen + ndist];
                let mut i = 0;
                while i < nlen + ndist {
                    let sym = ct.decode(&mut b)?;
                    let (val, rep) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => (*lens.get(i.checked_sub(1)?)?, 3 + b.need(2)? as usize),
                        17 => (0, 3 + b.need(3)? as usize),
                        _ => (0, 11 + b.need(7)? as usize),
                    };
                    if i + rep > nlen + ndist {
                        return None;
                    }
                    lens[i..i + rep].fill(val);
                    i += rep;
                }
                if lens[256] == 0 {
                    return None;
                }
                let lt = Huff::new(&lens[..nlen])?;
                let dt = Huff::new(&lens[nlen..])?;
                inflate_block(&mut b, &mut out, &lt, &dt, limit)?;
            }
            _ => return None,
        }
        if last == 1 {
            break;
        }
    }
    // the Adler-32 after the (byte-aligned) stream
    let p = b.pos - (b.nbits / 8) as usize;
    let c = data.get(p..p + 4)?;
    (u32::from_be_bytes([c[0], c[1], c[2], c[3]]) == adler32(&out)).then_some(out)
}

fn inflate_block(
    b: &mut Bits,
    out: &mut Vec<u8>,
    lt: &Huff,
    dt: &Huff,
    limit: usize,
) -> Option<()> {
    loop {
        let sym = usize::from(lt.decode(b)?);
        if sym < 256 {
            if out.len() >= limit {
                return None;
            }
            out.push(sym as u8);
        } else if sym == 256 {
            return Some(());
        } else {
            let s = sym - 257;
            if s >= 29 {
                return None;
            }
            let len = usize::from(LBASE[s]) + b.need(u32::from(LEXT[s]))? as usize;
            let d = usize::from(dt.decode(b)?);
            if d >= 30 {
                return None;
            }
            let dist = usize::from(DBASE[d]) + b.need(u32::from(DEXT[d]))? as usize;
            if dist > out.len() || out.len() + len > limit {
                return None;
            }
            let from = out.len() - dist;
            for k in 0..len {
                let v = out[from + k];
                out.push(v);
            }
        }
    }
}

struct BitOut {
    out: Vec<u8>,
    bit: u64,
    n: u32,
}

impl BitOut {
    fn put(&mut self, v: u32, n: u32) {
        self.bit |= u64::from(v) << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.bit as u8);
            self.bit >>= 8;
            self.n -= 8;
        }
    }
    /// A Huffman code (sent most significant bit first).
    fn code(&mut self, code: u32, len: u32) {
        let mut r = 0;
        for i in 0..len {
            r |= ((code >> i) & 1) << (len - 1 - i);
        }
        self.put(r, len);
    }
    fn lit(&mut self, v: usize) {
        match v {
            0..=143 => self.code(0x30 + v as u32, 8),
            144..=255 => self.code(0x190 + (v as u32 - 144), 9),
            256..=279 => self.code(v as u32 - 256, 7),
            _ => self.code(0xc0 + (v as u32 - 280), 8),
        }
    }
}

/// A zlib stream of `data` (fixed Huffman codes, greedy LZ77 matches) that
/// any zlib can read.
pub fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut w = BitOut {
        out: vec![0x78, 0x01],
        bit: 0,
        n: 0,
    };
    w.put(1, 1); // last block
    w.put(1, 2); // fixed Huffman
    const HBITS: u32 = 15;
    let mut head = vec![usize::MAX; 1 << HBITS];
    let hash = |p: usize| -> usize {
        let v = u32::from(data[p]) | (u32::from(data[p + 1]) << 8) | (u32::from(data[p + 2]) << 16);
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HBITS)) as usize
    };
    let mut i = 0;
    while i < data.len() {
        let mut best = (0usize, 0usize);
        if i + 3 <= data.len() {
            let h = hash(i);
            let cand = head[h];
            head[h] = i;
            if cand != usize::MAX && i - cand <= 32_768 {
                let max = (data.len() - i).min(258);
                let mut l = 0;
                while l < max && data[cand + l] == data[i + l] {
                    l += 1;
                }
                if l >= 3 {
                    best = (l, i - cand);
                }
            }
        }
        if best.0 >= 3 {
            let (len, dist) = best;
            let s = LBASE
                .iter()
                .rposition(|&b| usize::from(b) <= len)
                .expect("len >= 3");
            w.lit(257 + s);
            w.put((len - usize::from(LBASE[s])) as u32, u32::from(LEXT[s]));
            let d = DBASE
                .iter()
                .rposition(|&b| usize::from(b) <= dist)
                .expect("dist >= 1");
            w.code(d as u32, 5);
            w.put((dist - usize::from(DBASE[d])) as u32, u32::from(DEXT[d]));
            for p in i + 1..(i + len).min(data.len().saturating_sub(2)) {
                head[hash(p)] = p;
            }
            i += len;
        } else {
            w.lit(usize::from(data[i]));
            i += 1;
        }
    }
    w.lit(256);
    if w.n > 0 {
        w.out.push(w.bit as u8);
    }
    let mut out = w.out;
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

// ------------------------------------------------------------ word hash

const K1: u64 = 0x9E37_79B9_7F4A_7C15;
const K2: u64 = 0xC2B2_AE3D_27D4_EB4F;

/// One mixing step of [`WordHash`].
#[inline(always)]
fn mix(h: u64, w: u64) -> u64 {
    (h ^ w.wrapping_mul(K1)).rotate_left(31).wrapping_mul(K2)
}

/// A fast streaming 64-bit hash of words and bytes (deterministic across runs
/// and platforms). Words go alternately into two independent lanes (even and
/// odd word index), so two multiply chains overlap in the CPU. Byte input is
/// hashed as little-endian 8-byte words with the total length mixed in, so
/// how the bytes arrive (chunk sizes) does not matter. For detecting change,
/// not for security.
#[derive(Clone, Debug)]
pub struct WordHash {
    h: [u64; 2],
    /// Words mixed so far (its parity picks the lane of the next word).
    n: u64,
    tail: [u8; 8],
    tail_len: usize,
    bytes: u64,
}

impl Default for WordHash {
    fn default() -> Self {
        Self::new()
    }
}

impl WordHash {
    pub fn new() -> Self {
        WordHash {
            h: [0x6A09_E667_F3BC_C908, 0xBB67_AE85_84CA_A73B],
            n: 0,
            tail: [0; 8],
            tail_len: 0,
            bytes: 0,
        }
    }

    /// Mixes one word.
    #[inline(always)]
    pub fn word(&mut self, w: u64) {
        let lane = (self.n & 1) as usize;
        self.h[lane] = mix(self.h[lane], w);
        self.n += 1;
    }

    /// Mixes two words: the same as `word(a); word(b)`, faster when the word
    /// count so far is even.
    #[inline(always)]
    pub fn pair(&mut self, a: u64, b: u64) {
        if self.n & 1 == 0 {
            self.h[0] = mix(self.h[0], a);
            self.h[1] = mix(self.h[1], b);
            self.n += 2;
        } else {
            self.word(a);
            self.word(b);
        }
    }

    /// Mixes a run of words.
    #[inline]
    pub fn words(&mut self, ws: &[u64]) {
        let mut ws = ws;
        if self.n & 1 == 1
            && let Some((&w, rest)) = ws.split_first()
        {
            self.word(w);
            ws = rest;
        }
        let (mut h0, mut h1) = (self.h[0], self.h[1]);
        let mut pairs = ws.chunks_exact(2);
        for p in &mut pairs {
            h0 = mix(h0, p[0]);
            h1 = mix(h1, p[1]);
        }
        self.h = [h0, h1];
        self.n += 2 * (ws.len() / 2) as u64;
        if let [w] = pairs.remainder() {
            self.word(*w);
        }
    }

    /// Mixes bytes as a stream (independent of how they are split).
    pub fn bytes(&mut self, mut b: &[u8]) {
        self.bytes += b.len() as u64;
        if self.tail_len > 0 {
            let take = (8 - self.tail_len).min(b.len());
            self.tail[self.tail_len..self.tail_len + take].copy_from_slice(&b[..take]);
            self.tail_len += take;
            b = &b[take..];
            if self.tail_len < 8 {
                return;
            }
            self.word(u64::from_le_bytes(self.tail));
            self.tail_len = 0;
        }
        if self.n & 1 == 1 && b.len() >= 8 {
            self.word(u64::from_le_bytes(b[..8].try_into().expect("8 bytes")));
            b = &b[8..];
        }
        let (mut h0, mut h1) = (self.h[0], self.h[1]);
        let mut blocks = b.chunks_exact(16);
        for x in &mut blocks {
            h0 = mix(h0, u64::from_le_bytes(x[..8].try_into().expect("8 bytes")));
            h1 = mix(h1, u64::from_le_bytes(x[8..].try_into().expect("8 bytes")));
        }
        self.h = [h0, h1];
        self.n += 2 * (b.len() / 16) as u64;
        let mut rest = blocks.remainder();
        if rest.len() >= 8 {
            self.word(u64::from_le_bytes(rest[..8].try_into().expect("8 bytes")));
            rest = &rest[8..];
        }
        self.tail[..rest.len()].copy_from_slice(rest);
        self.tail_len = rest.len();
    }

    /// A length-delimited byte string (so ["ab", "c"] and ["a", "bc"]
    /// differ): its length as a word, then its bytes padded to a word.
    pub fn field(&mut self, b: &[u8]) {
        self.word(b.len() as u64);
        let mut words = b.chunks_exact(8);
        for w in &mut words {
            self.word(u64::from_le_bytes(w.try_into().expect("8 bytes")));
        }
        let rest = words.remainder();
        if !rest.is_empty() {
            let mut last = [0u8; 8];
            last[..rest.len()].copy_from_slice(rest);
            self.word(u64::from_le_bytes(last));
        }
    }

    /// Bytes fed through [`Self::bytes`] so far.
    pub fn byte_count(&self) -> u64 {
        self.bytes
    }

    /// The hash (the state is not consumed).
    pub fn finish(&self) -> u64 {
        let mut h = self.clone();
        if h.tail_len > 0 {
            let mut last = [0u8; 8];
            last[..h.tail_len].copy_from_slice(&h.tail[..h.tail_len]);
            h.word(u64::from_le_bytes(last));
        }
        let mut z = mix(h.h[0], h.h[1]) ^ h.n.wrapping_mul(K1) ^ h.bytes.wrapping_mul(K2);
        // SplitMix64 finish.
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zlib_and_base64_roundtrip() {
        let mut data = Vec::new();
        for i in 0..5000u32 {
            data.extend_from_slice(&(i % 97).to_le_bytes());
            data.extend_from_slice(&(i * 7).to_le_bytes());
        }
        let z = zlib_compress(&data);
        assert!(z.len() < data.len());
        assert!(zlib_compress(&[7u8; 10_000]).len() < 200);
        assert_eq!(zlib_decompress(&z, usize::MAX).unwrap(), data);
        let t = b64encode(&z);
        assert_eq!(b64decode_py(&t).unwrap(), z);
        assert_eq!(zlib_decompress(&zlib_compress(b""), 10).unwrap(), b"");
        assert!(zlib_decompress(&z, 100).is_none());
        // Python: zlib.compress(b"hello hello hello", 1)
        let py = [
            120u8, 1, 203, 72, 205, 201, 201, 87, 200, 64, 144, 0, 58, 46, 6, 125,
        ];
        assert_eq!(zlib_decompress(&py, 100).unwrap(), b"hello hello hello");
    }

    #[test]
    fn varint_roundtrip_and_lengths() {
        let values = [
            0u64,
            1,
            127,
            128,
            300,
            16_383,
            16_384,
            u64::from(u32::MAX),
            u64::MAX - 1,
            u64::MAX,
        ];
        let mut buf = Vec::new();
        for &v in &values {
            let before = buf.len();
            put_uvarint(&mut buf, v);
            assert_eq!(buf.len() - before, uvarint_len(v), "{v}");
        }
        let mut pos = 0;
        for &v in &values {
            assert_eq!(get_uvarint(&buf, &mut pos), Some(v));
        }
        assert_eq!(pos, buf.len());
        assert_eq!(get_uvarint(&buf, &mut pos), None);
        for v in [0i64, 1, -1, 63, -64, i64::MAX, i64::MIN] {
            let mut b = Vec::new();
            put_ivarint(&mut b, v);
            let mut p = 0;
            assert_eq!(get_ivarint(&b, &mut p), Some(v));
        }
    }

    #[test]
    fn varint_rejects_malformed() {
        // Truncated.
        assert_eq!(get_uvarint(&[0x80], &mut 0), None);
        // Eleven bytes.
        let long = [0xFFu8; 10];
        assert_eq!(get_uvarint(&[&long[..], &[0x01]].concat(), &mut 0), None);
        // Tenth byte carrying more than one bit overflows u64.
        let mut over = vec![0xFFu8; 9];
        over.push(0x02);
        assert_eq!(get_uvarint(&over, &mut 0), None);
    }

    #[test]
    fn crc32_known_values() {
        assert_eq!(Crc32::of(b""), 0);
        assert_eq!(Crc32::of(b"123456789"), 0xCBF4_3926);
        let mut c = Crc32::new();
        c.update(b"1234");
        c.update(b"56789");
        assert_eq!(c.finish(), 0xCBF4_3926);
    }
}
