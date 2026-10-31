//! Audio Reference (v0.8, contract §054–§061): a reference recording played
//! in sync with the project. Original implementation (dominohook has no
//! licence; only its behaviour was studied).
//!
//! Audio file → decoder (WAV here; FLAC `claxon`, Ogg Vorbis `lewton`, MP3
//! `puremp3`) → seekable PCM in memory (16-bit stereo at the file's rate,
//! about 10 MB a minute, bounded by a RAM budget) → project-time
//! synchroniser → mixed by the synth worker into PlumSpirit's own output
//! (same stream and clock as the synthesizer).
//!
//! Sync: the worker knows every block's playback frame. At speed `s` and
//! output rate `R`, project seconds = frame · s / R exactly (the tempo map of
//! the running audio is built for that speed), so the reference position is
//! worked out from the frame of each block — never accumulated — and
//! tempo changes, seeks, loops and pauses can never make it drift.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Columns of the waveform overview.
pub const PEAK_COLUMNS: usize = 4096;
/// Bytes per stereo frame in memory.
const FRAME_BYTES: u64 = 4;
/// Most file size read (compressed or not).
const MAX_FILE_BYTES: u64 = 2 << 30;
/// Audio file extensions offered and dropped.
pub const AUDIO_EXTENSIONS: [&str; 5] = ["wav", "flac", "ogg", "mp3", "wave"];

/// A decoded reference recording.
pub struct RefAudio {
    /// Sample rate of the file.
    pub rate: u32,
    pub frames: u64,
    /// Interleaved stereo.
    pcm: Vec<i16>,
    /// Waveform overview: (min, max) of the mono mix per column.
    pub peaks: Vec<[i16; 2]>,
    /// "WAV 24-bit", "FLAC 16-bit", "Ogg Vorbis", "MP3"…
    pub format: String,
    /// Channels in the file (shown; the reference plays in stereo).
    pub channels: u16,
}

impl std::fmt::Debug for RefAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RefAudio({}, {} Hz, {} frames)",
            self.format, self.rate, self.frames
        )
    }
}

impl RefAudio {
    pub fn duration_seconds(&self) -> f64 {
        self.frames as f64 / f64::from(self.rate.max(1))
    }
    pub fn memory_bytes(&self) -> u64 {
        self.pcm.len() as u64 * 2 + self.peaks.len() as u64 * 4
    }
    #[inline]
    fn at(&self, i: i64) -> (f32, f32) {
        if i < 0 || i as u64 >= self.frames {
            return (0.0, 0.0);
        }
        let k = i as usize * 2;
        (
            f32::from(self.pcm[k]) / 32768.0,
            f32::from(self.pcm[k + 1]) / 32768.0,
        )
    }
    /// The sound at a fractional frame (cubic Hermite / Catmull-Rom).
    pub fn sample(&self, pos: f64) -> (f32, f32) {
        if !(pos > -1.0 && pos < self.frames as f64) {
            return (0.0, 0.0);
        }
        let i = pos.floor() as i64;
        let t = (pos - pos.floor()) as f32;
        let (a, b, c, d) = (self.at(i - 1), self.at(i), self.at(i + 1), self.at(i + 2));
        let cr = |p0: f32, p1: f32, p2: f32, p3: f32| {
            p1 + 0.5
                * t
                * (p2 - p0
                    + t * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3 + t * (3.0 * (p1 - p2) + p3 - p0)))
        };
        (cr(a.0, b.0, c.0, d.0), cr(a.1, b.1, c.1, d.1))
    }
}

#[cfg(test)]
impl RefAudio {
    /// Silence with one 0.5 click (both sides) at each time `at` (seconds).
    pub fn test_clicks(rate: u32, seconds: f64, at: &[f64]) -> RefAudio {
        let frames = (seconds * f64::from(rate)) as usize;
        let mut pcm = Pcm::new(u64::MAX, frames as u64);
        pcm.data.resize(frames * 2, 0);
        for t in at {
            let i = (t * f64::from(rate)).round() as usize;
            if i < frames {
                pcm.data[2 * i] = 16384;
                pcm.data[2 * i + 1] = 16384;
            }
        }
        finish(pcm, rate, "test".into(), 2).expect("clicks")
    }
}

/// What the worker plays: the audio, where it starts in project time, its
/// gain and the playback speed.
#[derive(Clone, Debug)]
pub struct RefPlay {
    pub audio: Arc<RefAudio>,
    /// Project seconds where the reference's start plays.
    pub offset: f64,
    /// 0 – 2 (0 = muted).
    pub gain: f32,
    /// Playback speed (1 = normal).
    pub speed: f64,
}

impl RefPlay {
    /// The reference frame heard at playback frame `frame` (output rate `out`).
    #[inline]
    pub fn position(&self, frame: u64, out: u32) -> f64 {
        (frame as f64 * self.speed / f64::from(out.max(1)) - self.offset)
            * f64::from(self.audio.rate)
    }

    /// Adds the reference to one block (`inter`: interleaved stereo) whose
    /// first frame is playback frame `first`; `wrap`: (offset in the block,
    /// frame after a loop wrap).
    pub fn mix(&self, out: u32, first: u64, wrap: Option<(u32, u64)>, inter: &mut [f32]) {
        if self.gain <= 0.0 {
            return;
        }
        let n = inter.len() / 2;
        for i in 0..n {
            let frame = match wrap {
                Some((o, f)) if i as u32 >= o => f + (i as u64 - u64::from(o)),
                _ => first + i as u64,
            };
            let (l, r) = self.audio.sample(self.position(frame, out));
            inter[2 * i] += l * self.gain;
            inter[2 * i + 1] += r * self.gain;
        }
    }
}

/// Stereo 16-bit frames being collected, within a memory budget.
struct Pcm {
    data: Vec<i16>,
    budget_frames: u64,
}

impl Pcm {
    fn new(budget: u64, hint_frames: u64) -> Pcm {
        let budget_frames = budget / FRAME_BYTES;
        Pcm {
            data: Vec::with_capacity((hint_frames.min(budget_frames) as usize).saturating_mul(2)),
            budget_frames,
        }
    }
    #[inline]
    fn push(&mut self, l: f32, r: f32) -> Result<(), String> {
        if self.data.len() as u64 / 2 >= self.budget_frames {
            return Err(too_long(self.budget_frames));
        }
        let q = |x: f32| (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        self.data.push(q(l));
        self.data.push(q(r));
        Ok(())
    }
}

fn too_long(budget_frames: u64) -> String {
    format!(
        "The recording is too long for this PC's memory (the reference may use about {} MB).",
        budget_frames * FRAME_BYTES / (1 << 20)
    )
}

fn finish(pcm: Pcm, rate: u32, format: String, channels: u16) -> Result<RefAudio, String> {
    if rate == 0 {
        return Err("The recording has no sample rate.".into());
    }
    let frames = pcm.data.len() as u64 / 2;
    if frames == 0 {
        return Err("The recording has no sound in it.".into());
    }
    let mut peaks = vec![[0i16; 2]; PEAK_COLUMNS];
    let per = frames.div_ceil(PEAK_COLUMNS as u64).max(1);
    for (c, p) in peaks.iter_mut().enumerate() {
        let (a, b) = (c as u64 * per, ((c as u64 + 1) * per).min(frames));
        let (mut lo, mut hi) = (i16::MAX, i16::MIN);
        for f in a..b {
            let k = f as usize * 2;
            let m = ((i32::from(pcm.data[k]) + i32::from(pcm.data[k + 1])) / 2) as i16;
            lo = lo.min(m);
            hi = hi.max(m);
        }
        *p = if a < b { [lo, hi] } else { [0, 0] };
    }
    let mut data = pcm.data;
    data.shrink_to_fit();
    Ok(RefAudio {
        rate,
        frames,
        pcm: data,
        peaks,
        format,
        channels,
    })
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Stopped.".into())
    } else {
        Ok(())
    }
}

/// The memory a reference may use on this PC: an eighth of the RAM, 64 MB –
/// 1 GB (a 2 GB PC: about 25 minutes of CD-quality stereo).
pub fn memory_budget(total_ram: u64) -> u64 {
    (total_ram / 8).clamp(64 << 20, 1 << 30)
}

/// Decodes a file (see [`decode`]).
pub fn decode_file(path: &Path, budget: u64, cancel: &AtomicBool) -> Result<RefAudio, String> {
    let meta = std::fs::metadata(path).map_err(|e| {
        format!(
            "The file could not be read: {}",
            crate::audio::engine::sentence(&e)
        )
    })?;
    if meta.len() > MAX_FILE_BYTES {
        return Err("The file is too big for a reference (over 2 GB).".into());
    }
    let data = std::fs::read(path).map_err(|e| {
        format!(
            "The file could not be read: {}",
            crate::audio::engine::sentence(&e)
        )
    })?;
    decode(&data, budget, cancel)
}

/// Decodes a recording by its content (WAV, FLAC, Ogg Vorbis, MP3).
pub fn decode(data: &[u8], budget: u64, cancel: &AtomicBool) -> Result<RefAudio, String> {
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WAVE" {
        return decode_wav(data, budget, cancel);
    }
    if data.starts_with(b"RF64") || data.starts_with(b"BW64") {
        return Err("RF64 / BW64 WAV files are not supported; save the reference as an ordinary WAV, FLAC, Ogg or MP3.".into());
    }
    if data.starts_with(b"fLaC") || id3_end(data).is_some_and(|e| data[e..].starts_with(b"fLaC")) {
        return decode_flac(data, budget, cancel);
    }
    if data.starts_with(b"OggS") {
        return decode_ogg(data, budget, cancel);
    }
    if nanomp3::detect(data) {
        return decode_mp3(data, budget, cancel);
    }
    Err("This file is not a recording PlumSpirit can read (WAV, FLAC, Ogg Vorbis or MP3).".into())
}

// ------------------------------------------------------------------ WAV

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}

/// WAV: PCM 8 / 16 / 24 / 32-bit, float 32 / 64-bit, WAVE_FORMAT_EXTENSIBLE;
/// any number of channels (front left / right are kept; mono plays on both).
pub fn decode_wav(data: &[u8], budget: u64, cancel: &AtomicBool) -> Result<RefAudio, String> {
    let bad = |m: &str| format!("The WAV file is damaged: {m}.");
    let mut at = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16, u16)> = None; // tag, channels, rate, align, bits
    let mut body: Option<&[u8]> = None;
    while at + 8 <= data.len() {
        let id = &data[at..at + 4];
        let len = le32(data, at + 4).ok_or_else(|| bad("a chunk is cut short"))? as usize;
        let start = at + 8;
        let end = start.saturating_add(len).min(data.len());
        match id {
            b"fmt " => {
                let c = &data[start..end];
                let mut tag = le16(c, 0).ok_or_else(|| bad("the format is cut short"))?;
                let channels = le16(c, 2).unwrap_or(0);
                let rate = le32(c, 4).unwrap_or(0);
                let align = le16(c, 12).unwrap_or(0);
                let bits = le16(c, 14).unwrap_or(0);
                if tag == 0xFFFE {
                    // extensible: the sub-format GUID starts with the real tag
                    tag = le16(c, 24).ok_or_else(|| bad("the extensible format is cut short"))?;
                }
                fmt = Some((tag, channels, rate, align, bits));
            }
            b"data" => {
                body = Some(&data[start..end]);
                if fmt.is_some() {
                    break;
                }
            }
            _ => {}
        }
        // chunks are padded to an even length
        at = start.saturating_add(len).saturating_add(len & 1);
    }
    let (tag, channels, rate, align, bits) = fmt.ok_or_else(|| bad("it has no format chunk"))?;
    let body = body.ok_or_else(|| bad("it has no sound data"))?;
    if channels == 0 || rate == 0 {
        return Err(bad("no channels or no sample rate"));
    }
    let width = match (tag, bits) {
        (1, 8) => 1,
        (1, 16) => 2,
        (1, 24) => 3,
        (1, 32) | (3, 32) => 4,
        (3, 64) => 8,
        (1 | 3, b) => {
            return Err(format!(
                "{b}-bit WAV files are not supported (8, 16, 24 and 32-bit PCM, 32 and 64-bit float are)."
            ));
        }
        (t, _) => {
            return Err(format!(
                "This WAV file is compressed (format {t:#06x}); save it as PCM WAV, FLAC, Ogg or MP3."
            ));
        }
    };
    let frame_bytes = usize::from(channels) * width;
    let align = usize::from(align).max(frame_bytes);
    let frames = body.len() / align;
    let mut pcm = Pcm::new(budget, frames as u64);
    let read = |s: &[u8]| -> f32 {
        match (tag, width) {
            (1, 1) => (f32::from(s[0]) - 128.0) / 128.0,
            (1, 2) => f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0,
            (1, 3) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
            (1, 4) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
            (3, 4) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            _ => f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]) as f32,
        }
    };
    for f in 0..frames {
        if f % 65_536 == 0 {
            cancelled(cancel)?;
        }
        let s = &body[f * align..f * align + frame_bytes];
        let l = read(&s[..width]);
        let r = if channels > 1 {
            read(&s[width..2 * width])
        } else {
            l
        };
        pcm.push(clean(l), clean(r))?;
    }
    let kind = if tag == 3 { "float" } else { "PCM" };
    finish(pcm, rate, format!("WAV {bits}-bit {kind}"), channels)
}

#[inline]
fn clean(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

// ------------------------------------------------------------------ FLAC

fn decode_flac(data: &[u8], budget: u64, cancel: &AtomicBool) -> Result<RefAudio, String> {
    let start = id3_end(data).unwrap_or(0);
    let mut reader = claxon::FlacReader::new(std::io::Cursor::new(&data[start..]))
        .map_err(|e| format!("The FLAC file could not be read: {e}."))?;
    let info = reader.streaminfo();
    let channels = info.channels as usize;
    let bits = info.bits_per_sample;
    if channels == 0 || bits == 0 || bits > 32 {
        return Err("The FLAC file has an unusual format PlumSpirit cannot read.".into());
    }
    let scale = 1.0 / (1u64 << (bits - 1)) as f32;
    let mut pcm = Pcm::new(budget, info.samples.unwrap_or(0));
    let mut frame = Vec::with_capacity(channels);
    for (n, s) in reader.samples().enumerate() {
        if n % 262_144 == 0 {
            cancelled(cancel)?;
        }
        let s = s.map_err(|e| format!("The FLAC file is damaged: {e}."))?;
        frame.push(s as f32 * scale);
        if frame.len() == channels {
            let (l, r) = (frame[0], if channels > 1 { frame[1] } else { frame[0] });
            pcm.push(l, r)?;
            frame.clear();
        }
    }
    finish(
        pcm,
        info.sample_rate,
        format!("FLAC {bits}-bit"),
        channels as u16,
    )
}

// ------------------------------------------------------------------ Ogg Vorbis

fn decode_ogg(data: &[u8], budget: u64, cancel: &AtomicBool) -> Result<RefAudio, String> {
    let mut reader =
        lewton::inside_ogg::OggStreamReader::new(std::io::Cursor::new(data)).map_err(|e| {
            format!("The Ogg file could not be read (only Ogg Vorbis is supported): {e}.")
        })?;
    let channels = usize::from(reader.ident_hdr.audio_channels);
    let rate = reader.ident_hdr.audio_sample_rate;
    if channels == 0 {
        return Err("The Ogg file has no channels.".into());
    }
    let mut pcm = Pcm::new(budget, 0);
    let mut n = 0usize;
    loop {
        n += 1;
        if n.is_multiple_of(256) {
            cancelled(cancel)?;
        }
        match reader.read_dec_packet_itl() {
            Ok(Some(samples)) => {
                for f in samples.chunks_exact(channels) {
                    let l = f32::from(f[0]) / 32768.0;
                    let r = if channels > 1 {
                        f32::from(f[1]) / 32768.0
                    } else {
                        l
                    };
                    pcm.push(l, r)?;
                }
            }
            Ok(None) => break,
            Err(e) => {
                if pcm.data.is_empty() {
                    return Err(format!("The Ogg file is damaged: {e}."));
                }
                break; // a damaged tail: keep what was read
            }
        }
    }
    // the last page's granule position is the exact length (the last packet
    // is decoded whole)
    if let Some(gp) = reader.get_last_absgp() {
        let keep = (gp as usize).saturating_mul(2);
        if keep > 0 && keep < pcm.data.len() {
            pcm.data.truncate(keep);
        }
    }
    finish(pcm, rate, "Ogg Vorbis".into(), channels as u16)
}

// ------------------------------------------------------------------ MP3

/// End of an ID3v2 tag at the start, if there is one.
fn id3_end(data: &[u8]) -> Option<usize> {
    if data.len() < 10 || &data[0..3] != b"ID3" {
        return None;
    }
    let size = data[6..10]
        .iter()
        .fold(0usize, |a, b| (a << 7) | usize::from(b & 0x7F));
    let footer = if data[5] & 0x10 != 0 { 10 } else { 0 };
    Some((10 + size + footer).min(data.len()))
}

/// MP3 (and MP1 / MP2) through nanomp3: tags skipped, encoder delay and
/// padding trimmed (gapless), like minimp3.
fn decode_mp3(data: &[u8], budget: u64, cancel: &AtomicBool) -> Result<RefAudio, String> {
    let mut r = nanomp3::SliceReader::<f32>::new(data);
    let rate = r.sample_rate();
    let channels = match r.channels() {
        Some(c) => c as usize,
        None => return Err("No MP3 sound was found in the file.".into()),
    };
    if rate == 0 || channels == 0 {
        return Err("No MP3 sound was found in the file.".into());
    }
    let mut pcm = Pcm::new(budget, r.total_samples().unwrap_or(0));
    let mut n = 0usize;
    loop {
        n += 1;
        if n.is_multiple_of(512) {
            cancelled(cancel)?;
        }
        match r.read_frame() {
            Ok(Some(s)) => {
                for f in s.chunks_exact(channels) {
                    let l = clean(f[0]);
                    let rr = if channels > 1 { clean(f[1]) } else { l };
                    pcm.push(l, rr)?;
                }
            }
            Ok(None) => break,
            Err(e) => {
                if pcm.data.is_empty() {
                    return Err(format!("The MP3 file is damaged: {e}."));
                }
                break; // a damaged tail: keep what was read
            }
        }
    }
    finish(pcm, rate, "MP3".into(), channels as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(
        tag: u16,
        channels: u16,
        rate: u32,
        bits: u16,
        body: &[u8],
        extensible: bool,
    ) -> Vec<u8> {
        let mut fmt = Vec::new();
        let align = channels * bits / 8;
        fmt.extend_from_slice(&(if extensible { 0xFFFE } else { tag }).to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&rate.to_le_bytes());
        fmt.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
        fmt.extend_from_slice(&align.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        if extensible {
            fmt.extend_from_slice(&22u16.to_le_bytes());
            fmt.extend_from_slice(&bits.to_le_bytes());
            fmt.extend_from_slice(&3u32.to_le_bytes());
            fmt.extend_from_slice(&tag.to_le_bytes());
            fmt.extend_from_slice(&[0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
        }
        let mut out = b"RIFF\0\0\0\0WAVE".to_vec();
        // a chunk to skip first (odd length, padded)
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&[1, 2, 3, 0]);
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        out.extend_from_slice(&fmt);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn no() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn every_wav_sample_format_reads_the_same_sound() {
        // one frame of -0.5 / +0.25 in each format
        let cases: Vec<(u16, u16, Vec<u8>)> = vec![
            (
                1,
                16,
                [(-16384i16).to_le_bytes(), 8192i16.to_le_bytes()].concat(),
            ),
            (1, 24, vec![0x00, 0x00, 0xC0, 0x00, 0x00, 0x20]),
            (
                1,
                32,
                [
                    (-1_073_741_824i32).to_le_bytes(),
                    536_870_912i32.to_le_bytes(),
                ]
                .concat(),
            ),
            (
                3,
                32,
                [(-0.5f32).to_le_bytes(), 0.25f32.to_le_bytes()].concat(),
            ),
            (
                3,
                64,
                [(-0.5f64).to_le_bytes(), 0.25f64.to_le_bytes()].concat(),
            ),
            (1, 8, vec![64, 160]),
        ];
        for ext in [false, true] {
            for (tag, bits, body) in &cases {
                let a = decode(&wav(*tag, 2, 44_100, *bits, body, ext), 1 << 20, &no())
                    .unwrap_or_else(|e| panic!("{tag} {bits}: {e}"));
                assert_eq!(a.frames, 1);
                assert_eq!(a.rate, 44_100);
                let (l, r) = a.at(0);
                assert!(
                    (l + 0.5).abs() < 1e-4 && (r - 0.25).abs() < 1e-4,
                    "{tag} {bits}: {l} {r}"
                );
            }
        }
    }

    #[test]
    fn mono_plays_on_both_sides_and_more_channels_keep_front_left_right() {
        let mono = decode(
            &wav(1, 1, 8000, 16, &1000i16.to_le_bytes(), false),
            1 << 20,
            &no(),
        )
        .unwrap();
        assert_eq!(mono.at(0), (1000.0 / 32768.0, 1000.0 / 32768.0));
        let body: Vec<u8> = [100i16, 200, 300, 400, 500, 600]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let six = decode(&wav(1, 6, 8000, 16, &body, true), 1 << 20, &no()).unwrap();
        assert_eq!(six.channels, 6);
        assert_eq!(six.at(0), (100.0 / 32768.0, 200.0 / 32768.0));
    }

    #[test]
    fn broken_and_unsupported_files_give_sentences() {
        for (data, want) in [
            (b"RIFF\0\0\0\0WAVEdata\x04\0\0\0abcd".to_vec(), "format"),
            (wav(2, 2, 8000, 4, &[0; 8], false), "compressed"),
            (wav(1, 2, 8000, 12, &[0; 8], false), "12-bit"),
            (wav(1, 2, 8000, 16, &[], false), "no sound"),
            (b"hello, not audio".to_vec(), "not a recording"),
        ] {
            let e = decode(&data, 1 << 20, &no()).unwrap_err();
            assert!(e.contains(want), "{e}");
            assert!(e.ends_with('.'), "{e}");
        }
    }

    #[test]
    fn the_memory_budget_refuses_long_recordings_before_using_more() {
        let body = vec![0u8; 4 * 1000];
        let e = decode(&wav(1, 2, 8000, 16, &body, false), 4 * 999, &no()).unwrap_err();
        assert!(e.contains("too long"), "{e}");
        assert_eq!(memory_budget(2 << 30), 256 << 20);
        assert_eq!(memory_budget(64 << 30), 1 << 30);
        assert_eq!(memory_budget(256 << 20), 64 << 20);
    }

    fn ramp(frames: usize, rate: u32) -> Arc<RefAudio> {
        // left = frame number / 32768 (exact in 16 bits), right = −left
        let body: Vec<u8> = (0..frames)
            .flat_map(|f| {
                let v = (f % 30_000) as i16;
                [v.to_le_bytes(), (-v).to_le_bytes()].concat()
            })
            .collect();
        Arc::new(decode(&wav(1, 2, rate, 16, &body, false), 1 << 30, &no()).unwrap())
    }

    #[test]
    fn the_reference_follows_the_playback_frame_with_offset_and_speed() {
        let a = ramp(20_000, 8_000);
        let p = RefPlay {
            audio: a.clone(),
            offset: 0.5,
            gain: 1.0,
            speed: 1.0,
        };
        // output 48 kHz: frame 48,000 = project 1 s = reference 0.5 s = frame 4,000
        assert!((p.position(48_000, 48_000) - 4_000.0).abs() < 1e-9);
        // before the reference starts: silence
        let mut b = vec![0f32; 128];
        p.mix(48_000, 0, None, &mut b);
        assert!(b.iter().all(|x| *x == 0.0));
        // double speed: project time runs twice as fast
        let p2 = RefPlay {
            speed: 2.0,
            ..p.clone()
        };
        assert!((p2.position(48_000, 48_000) - 12_000.0).abs() < 1e-9);
        // exact samples land on their own value
        let mut b = vec![0f32; 2];
        let p3 = RefPlay {
            offset: 0.0,
            ..p.clone()
        };
        p3.mix(8_000, 1234, None, &mut b);
        assert!((b[0] - 1234.0 / 32768.0).abs() < 1e-6 && (b[1] + 1234.0 / 32768.0).abs() < 1e-6);
    }

    #[test]
    fn a_thousand_loop_wraps_land_on_the_same_reference_sample() {
        // loop of 1.5 s at 48 kHz; every wrap restarts at the loop's frame
        let a = ramp(30_000, 8_000);
        let p = RefPlay {
            audio: a,
            offset: 0.0,
            gain: 1.0,
            speed: 1.0,
        };
        let (start, len) = (24_000u64, 72_000u64); // 0.5 s .. 2.0 s
        let mut first = start;
        let mut heard = Vec::new();
        let mut block = vec![0f32; 128];
        for _ in 0..1000 {
            // the block that crosses the loop end wraps 10 frames in
            let wrap_at = 10u32;
            let pre = start + len - u64::from(wrap_at);
            block.iter_mut().for_each(|x| *x = 0.0);
            p.mix(48_000, pre, Some((wrap_at, start)), &mut block);
            heard.push(block[2 * wrap_at as usize]);
            first = start;
        }
        let _ = first;
        let want = (start as f64 * 8_000.0 / 48_000.0) as f32 / 32768.0;
        assert!(
            heard.iter().all(|h| (h - want).abs() < 1e-6),
            "{:?}",
            &heard[..3]
        );
    }

    #[test]
    fn muted_references_add_nothing_and_gain_scales() {
        let a = ramp(1000, 8_000);
        let mut p = RefPlay {
            audio: a,
            offset: 0.0,
            gain: 0.0,
            speed: 1.0,
        };
        let mut b = vec![0f32; 64];
        p.mix(8_000, 100, None, &mut b);
        assert!(b.iter().all(|x| *x == 0.0));
        p.gain = 2.0;
        p.mix(8_000, 100, None, &mut b);
        assert!((b[0] - 2.0 * 100.0 / 32768.0).abs() < 1e-6);
    }

    #[test]
    fn the_overview_has_the_loudest_moments() {
        let a = ramp(20_000, 8_000);
        assert_eq!(a.peaks.len(), PEAK_COLUMNS);
        // mono mix of +v / −v is silence
        assert!(a.peaks.iter().all(|p| p[0] == 0 && p[1] == 0));
        let body: Vec<u8> = (0..8000)
            .flat_map(|f| (if f == 4000 { 30_000i16 } else { 0 }).to_le_bytes())
            .collect();
        let b = decode(&wav(1, 1, 8_000, 16, &body, false), 1 << 20, &no()).unwrap();
        assert_eq!(b.peaks.iter().map(|p| p[1]).max(), Some(30_000));
    }

    #[test]
    fn decoding_stops_when_asked() {
        let body = vec![0u8; 4 * 200_000];
        let stop = AtomicBool::new(true);
        assert_eq!(
            decode(&wav(1, 2, 8000, 16, &body, false), 1 << 30, &stop).unwrap_err(),
            "Stopped."
        );
    }

    #[test]
    fn id3_tags_are_skipped_before_flac() {
        let d = b"ID3\x04\0\0\0\0\0\x05xxxxx".to_vec();
        assert_eq!(id3_end(&d), Some(15));
        assert_eq!(id3_end(b"fLaC"), None);
    }

    /// The test recordings: 0.6 s with a 5 ms 2 kHz burst (Hann window,
    /// 0.8) at 0.25 s, mono — WAV / FLAC / Ogg Vorbis / MP3 (MPEG-2) at
    /// 22.05 kHz and MP3 (MPEG-1) at 44.1 kHz, made with ffmpeg (LAME,
    /// libvorbis). The burst is found by correlation with the formula.
    fn burst_lag(a: &RefAudio) -> i64 {
        let rate = f64::from(a.rate);
        let start = (0.25 * rate) as i64;
        let len = (0.005 * rate) as i64;
        let want = |i: i64| {
            let k = i - start;
            if !(0..len).contains(&k) {
                return 0.0;
            }
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * k as f64 / (len - 1) as f64).cos();
            0.8 * w * (2.0 * std::f64::consts::PI * 2000.0 * k as f64 / rate).sin()
        };
        (-200i64..=200)
            .max_by(|x, y| {
                let c = |lag: i64| {
                    (start..start + len)
                        .map(|i| want(i) * f64::from(a.at(i + lag).0))
                        .sum::<f64>()
                };
                c(*x).total_cmp(&c(*y))
            })
            .unwrap()
    }

    /// A test recording from `fixtures/codecs.b64`.
    fn fixture(name: &str) -> Vec<u8> {
        include_str!("fixtures/codecs.b64")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
            .and_then(plumspirit_engine::codec::b64decode_py)
            .unwrap_or_else(|| panic!("fixture {name}"))
    }

    #[test]
    fn every_format_puts_the_sound_at_the_same_time() {
        for (name, rate, tol) in [
            ("burst.wav", 22_050, 0),
            ("burst.flac", 22_050, 0),
            ("burst.ogg", 22_050, 2),
            ("burst.mp3", 22_050, 2),
            ("b44.mp3", 44_100, 2),
        ] {
            let a =
                decode(&fixture(name), 1 << 24, &no()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(a.rate, rate, "{name}");
            assert_eq!(a.channels, 1, "{name}");
            let lag = burst_lag(&a);
            assert!(lag.abs() <= tol, "{name}: the burst is {lag} samples off");
            let want = (0.6 * f64::from(rate)).round() as i64;
            assert!(
                (a.frames as i64 - want).abs() <= 2,
                "{name}: {} frames, not {want}",
                a.frames
            );
        }
    }
}
