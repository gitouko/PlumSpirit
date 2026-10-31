//! Streaming Standard MIDI File reader.
//!
//! The reader never loads the whole file: it pulls fixed-size blocks from any
//! `std::io::Read`, tracks the absolute byte offset for precise error messages, and
//! emits tokens to a [`SmfHandler`]. The importer (building a `PlumDocument`), the
//! export validator (comparing against the document) and the semantic comparison
//! used by tests are all handlers over this single tokenizer.
//!
//! # Untrusted input
//!
//! Every length read from the file is checked against the remaining bytes of its
//! chunk and of the file before any allocation, variable-length quantities are
//! limited to four bytes, and tick accumulation uses checked arithmetic. Malformed
//! input produces a [`MidiError`], never a panic.
//!
//! # Running status
//!
//! Channel messages may omit the status byte when it repeats ("running status"). The
//! specification says meta and SysEx events cancel running status; some writers
//! ignore that, so the reader keeps the previous channel status across them and
//! reports a warning. Files that follow the specification are unaffected.
//!
//! # Compatibility mode (v1.0-Launch L-D05, contract §015)
//!
//! A handler that answers [`SmfHandler::compatibility`] with true gets three
//! deterministic repairs of deliberately out-of-spec files, each reported as
//! an [`ImportWarning`] with its track and byte offset:
//!
//! * a variable-length quantity longer than four bytes (a delta-time or a
//!   length) is read to its exact value, up to ten bytes / 64 bits
//!   ([`ImportWarning::LongVlq`]); a length must still fit its chunk;
//! * an event that runs past its chunk's declared end is dropped and the
//!   track ends at that end — nothing after a chunk is read as part of it
//!   ([`ImportWarning::EventCutAtChunkEnd`]);
//! * a file that ends inside a track keeps every complete event; the cut
//!   event is dropped and the track ends there ([`ImportWarning::TrackCutOff`]).
//!
//! Everything else is refused exactly as in the strict reading, which is
//! unchanged.

use super::error::{ImportWarning, MidiError, MidiErrorKind};
use crate::model::events::{channel_data_len, meta, system_data_len};
use std::io::Read;

const BLOCK: usize = 256 * 1024;
/// How often (in input bytes) the handler's progress hook runs.
const PROGRESS_INTERVAL: u64 = 256 * 1024;

/// Buffered byte source with absolute offset tracking.
pub struct ByteSource<R> {
    inner: R,
    buf: Vec<u8>,
    pos: usize,
    filled: usize,
    base: u64,
    total_len: Option<u64>,
}

impl<R: Read> ByteSource<R> {
    /// `total_len` is the size of the input when known (it lets the reader reject
    /// impossible lengths before allocating).
    pub fn new(inner: R, total_len: Option<u64>) -> Self {
        ByteSource::with_base(inner, 0, total_len)
    }

    /// A source whose first byte sits at file offset `base` (v1.0-beta1
    /// B1-0161: a track chunk decoded on a worker reports the same positions
    /// as when it is decoded in place). `total_len` is the whole file's length.
    pub fn with_base(inner: R, base: u64, total_len: Option<u64>) -> Self {
        ByteSource {
            inner,
            buf: vec![0; BLOCK],
            pos: 0,
            filled: 0,
            base,
            total_len,
        }
    }

    #[inline]
    pub fn position(&self) -> u64 {
        self.base + self.pos as u64
    }

    pub fn total_len(&self) -> Option<u64> {
        self.total_len
    }

    /// Ensures at least `want` bytes are buffered if the input has them. Returns the
    /// number of bytes available (may be less at end of input).
    fn ensure(&mut self, want: usize) -> Result<usize, MidiError> {
        debug_assert!(want <= BLOCK);
        let avail = self.filled - self.pos;
        if avail >= want {
            return Ok(avail);
        }
        // Move the unread tail to the front, then refill.
        self.buf.copy_within(self.pos..self.filled, 0);
        self.base += self.pos as u64;
        self.filled = avail;
        self.pos = 0;
        while self.filled < want {
            let n = match self.inner.read(&mut self.buf[self.filled..]) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(MidiError::from(e)),
            };
            if n == 0 {
                break;
            }
            self.filled += n;
        }
        Ok(self.filled - self.pos)
    }

    #[inline]
    pub fn read_u8(&mut self) -> Result<u8, MidiError> {
        if self.pos < self.filled {
            let b = self.buf[self.pos];
            self.pos += 1;
            return Ok(b);
        }
        if self.ensure(1)? == 0 {
            return Err(MidiError::at(MidiErrorKind::UnexpectedEof, self.position()));
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    pub fn read_exact(&mut self, dst: &mut [u8]) -> Result<(), MidiError> {
        let mut done = 0;
        while done < dst.len() {
            let avail = self.ensure(1)?;
            if avail == 0 {
                return Err(MidiError::at(MidiErrorKind::UnexpectedEof, self.position()));
            }
            let n = avail.min(dst.len() - done);
            dst[done..done + n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            done += n;
        }
        Ok(())
    }

    pub fn skip(&mut self, mut n: u64) -> Result<(), MidiError> {
        while n > 0 {
            let avail = self.ensure(1)?;
            if avail == 0 {
                return Err(MidiError::at(MidiErrorKind::UnexpectedEof, self.position()));
            }
            let k = (avail as u64).min(n) as usize;
            self.pos += k;
            n -= k as u64;
        }
        Ok(())
    }

    /// Returns up to `N` upcoming bytes without consuming them.
    pub fn peek<const N: usize>(&mut self) -> Result<Option<[u8; N]>, MidiError> {
        if self.ensure(N)? < N {
            return Ok(None);
        }
        let mut out = [0u8; N];
        out.copy_from_slice(&self.buf[self.pos..self.pos + N]);
        Ok(Some(out))
    }

    pub fn at_eof(&mut self) -> Result<bool, MidiError> {
        Ok(self.ensure(1)? == 0)
    }

    fn read_u16_be(&mut self) -> Result<u16, MidiError> {
        let mut b = [0u8; 2];
        self.read_exact(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }

    fn read_u32_be(&mut self) -> Result<u32, MidiError> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b)?;
        Ok(u32::from_be_bytes(b))
    }
}

/// One decoded event from a track chunk. Payload slices borrow the reader's scratch
/// buffer and are only valid during the handler call.
#[derive(Debug, PartialEq, Eq)]
pub enum Token<'a> {
    Channel {
        tick: u64,
        status: u8,
        data: [u8; 2],
    },
    Meta {
        tick: u64,
        meta_type: u8,
        payload: &'a [u8],
    },
    SysEx {
        tick: u64,
        payload: &'a [u8],
    },
    SysExEscape {
        tick: u64,
        payload: &'a [u8],
    },
    System {
        tick: u64,
        status: u8,
        data: [u8; 2],
    },
}

impl Token<'_> {
    pub fn tick(&self) -> u64 {
        match *self {
            Token::Channel { tick, .. }
            | Token::Meta { tick, .. }
            | Token::SysEx { tick, .. }
            | Token::SysExEscape { tick, .. }
            | Token::System { tick, .. } => tick,
        }
    }
}

/// Receives the structure and events of a Standard MIDI File.
pub trait SmfHandler {
    fn header(&mut self, format: u16, declared_tracks: u16, division: u16)
    -> Result<(), MidiError>;
    fn begin_track(&mut self, index: usize) -> Result<(), MidiError>;
    fn token(&mut self, token: Token<'_>) -> Result<(), MidiError>;
    /// `end_of_track_tick` is `None` when the chunk had no End-of-Track event.
    fn end_track(&mut self, index: usize, end_of_track_tick: Option<u64>) -> Result<(), MidiError>;
    fn extra_chunk(
        &mut self,
        chunk_type: [u8; 4],
        data: Vec<u8>,
        after_track_chunks: usize,
    ) -> Result<(), MidiError>;
    fn warning(&mut self, warning: ImportWarning);
    /// Called periodically with the current byte offset; may return `Cancelled`.
    fn progress(&mut self, _position: u64) -> Result<(), MidiError> {
        Ok(())
    }
    /// v1.0-Launch L-D05: compatibility reading (the three repairs in the
    /// module documentation). Off (the default): the strict reading.
    fn compatibility(&self) -> bool {
        false
    }
    /// v0.9 §062: read non-standard 256-key files (Kiva's rule) — the key byte
    /// of a Note On / Off with an **explicit** status byte is taken unmasked,
    /// so 0x80–0xFF are keys 128–255. Under running status a byte ≥ 0x80 is
    /// always a status byte. Off (the default): such a byte refuses the file
    /// with [`MidiErrorKind::WideKeyNote`].
    fn extended_keys(&self) -> bool {
        false
    }
    /// v1.0-beta1 (B1-0161): a handler that decodes track chunks elsewhere
    /// (on worker threads) returns true to take track `index`, a complete
    /// `MTrk` chunk of `len` data bytes lying wholly inside the file, as bytes:
    /// [`read_smf`] then reads them and passes them to
    /// [`SmfHandler::track_bytes`] instead of decoding them in place (no
    /// `begin_track` / `token` / `end_track` calls for that track). The
    /// handler decodes them with [`decode_track_bytes`].
    fn defer_track(&mut self, _index: usize, _len: u64) -> bool {
        false
    }
    /// The data bytes of a track taken by [`SmfHandler::defer_track`];
    /// `data_start` is the file offset of the first one.
    fn track_bytes(
        &mut self,
        _index: usize,
        _data_start: u64,
        _bytes: Vec<u8>,
    ) -> Result<(), MidiError> {
        Ok(())
    }
}

/// Decodes the data bytes of one complete `MTrk` chunk (taken through
/// [`SmfHandler::defer_track`]) exactly as [`read_smf`] decodes a chunk in
/// place: the same tokens, warnings and errors at the same file positions
/// (`data_start` is the offset of `bytes[0]`, `file_len` the file's length).
/// Returns the End of Track tick (`None` when the chunk has none).
pub fn decode_track_bytes<H: SmfHandler>(
    bytes: &[u8],
    data_start: u64,
    file_len: Option<u64>,
    track: usize,
    handler: &mut H,
) -> Result<Option<u64>, MidiError> {
    let mut src = ByteSource::with_base(bytes, data_start, file_len);
    let mut scratch = Vec::new();
    read_track(&mut src, bytes.len() as u64, track, &mut scratch, handler)
        .map(|o| o.end_of_track_tick)
        .map_err(|e| e.in_track(track))
}

fn is_chunk_type(t: &[u8; 4]) -> bool {
    t.iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b' ' || *b == b'_' || *b == b'-')
}

/// Reads an entire Standard MIDI File, feeding `handler`.
pub fn read_smf<R: Read, H: SmfHandler>(
    src: &mut ByteSource<R>,
    handler: &mut H,
) -> Result<(), MidiError> {
    // ---- header chunk
    let mut magic = [0u8; 4];
    if src.ensure(4)? < 4 {
        return Err(MidiError::at(MidiErrorKind::NotMidi, 0));
    }
    src.read_exact(&mut magic)?;
    if &magic != b"MThd" {
        return Err(MidiError::at(MidiErrorKind::NotMidi, 0));
    }
    let hlen = src
        .read_u32_be()
        .map_err(|_| MidiError::at(MidiErrorKind::TruncatedHeader, 4))?;
    if hlen < 6 {
        return Err(MidiError::at(MidiErrorKind::TruncatedHeader, 4));
    }
    let format = src
        .read_u16_be()
        .map_err(|_| MidiError::at(MidiErrorKind::TruncatedHeader, 8))?;
    let ntracks = src
        .read_u16_be()
        .map_err(|_| MidiError::at(MidiErrorKind::TruncatedHeader, 10))?;
    let division = src
        .read_u16_be()
        .map_err(|_| MidiError::at(MidiErrorKind::TruncatedHeader, 12))?;
    if format > 2 {
        return Err(MidiError::at(MidiErrorKind::UnsupportedFormat(format), 8));
    }
    if hlen > 6 {
        let extra = u64::from(hlen - 6);
        src.skip(extra)
            .map_err(|_| MidiError::at(MidiErrorKind::TruncatedHeader, 14))?;
        handler.warning(ImportWarning::LongHeader { extra });
    }
    handler.header(format, ntracks, division)?;

    // ---- chunks
    let mut track_index = 0usize;
    let mut scratch: Vec<u8> = Vec::new();
    loop {
        if src.at_eof()? {
            break;
        }
        let chunk_start = src.position();
        let header = match src.peek::<8>()? {
            Some(h) => h,
            None => {
                let rest = src.total_len().map_or(0, |t| t.saturating_sub(chunk_start));
                handler.warning(ImportWarning::TrailingBytes { bytes: rest.max(1) });
                break;
            }
        };
        let chunk_type = [header[0], header[1], header[2], header[3]];
        let len = u64::from(u32::from_be_bytes([
            header[4], header[5], header[6], header[7],
        ]));
        if &chunk_type == b"MTrk" {
            src.skip(8)?;
            let data_start = chunk_start + 8;
            let whole = src.total_len().is_some_and(|t| data_start + len <= t);
            if whole && handler.defer_track(track_index, len) {
                let mut data = Vec::new();
                data.try_reserve_exact(len as usize)
                    .map_err(|_| MidiError::new(MidiErrorKind::OutOfMemory))?;
                data.resize(len as usize, 0);
                src.read_exact(&mut data)?;
                handler.track_bytes(track_index, data_start, data)?;
            } else {
                handler.begin_track(track_index)?;
                let outcome = read_track(src, len, track_index, &mut scratch, handler)
                    .map_err(|e| e.in_track(track_index))?;
                handler.end_track(track_index, outcome.end_of_track_tick)?;
            }
            track_index += 1;
        } else if is_chunk_type(&chunk_type) {
            let data_start = chunk_start + 8;
            let fits = src.total_len().is_none_or(|t| data_start + len <= t);
            if !fits {
                let rest = src.total_len().map_or(0, |t| t.saturating_sub(chunk_start));
                handler.warning(ImportWarning::TrailingBytes { bytes: rest });
                break;
            }
            src.skip(8)?;
            let mut data = Vec::new();
            data.try_reserve_exact(len as usize)
                .map_err(|_| MidiError::new(MidiErrorKind::OutOfMemory))?;
            data.resize(len as usize, 0);
            src.read_exact(&mut data)?;
            handler.warning(ImportWarning::UnknownChunk {
                chunk_type,
                bytes: len,
            });
            handler.extra_chunk(chunk_type, data, track_index)?;
        } else {
            let rest = src.total_len().map_or(0, |t| t.saturating_sub(chunk_start));
            handler.warning(ImportWarning::TrailingBytes { bytes: rest.max(1) });
            break;
        }
    }
    if track_index == 0 {
        return Err(MidiError::new(MidiErrorKind::NoTracks));
    }
    if usize::from(ntracks) != track_index {
        handler.warning(ImportWarning::TrackCountMismatch {
            declared: ntracks,
            found: track_index,
        });
    }
    Ok(())
}

struct TrackOutcome {
    end_of_track_tick: Option<u64>,
}

/// The longest variable-length quantity compatibility mode reads (ten
/// bytes carry 70 bits; the value must still fit 64).
pub const COMPAT_VLQ_BYTES: u32 = 10;

/// What one track's reading counted, for the warnings at its end.
#[derive(Default)]
struct TrackState {
    lenient_running: usize,
    system_count: usize,
    /// File offset of the event being read (its delta-time).
    event_start: u64,
    /// Compatibility mode: quantities longer than four bytes.
    long_vlq: u64,
    long_first: u64,
    long_bytes: u32,
    long_largest: u64,
}

fn read_track<R: Read, H: SmfHandler>(
    src: &mut ByteSource<R>,
    chunk_len: u64,
    track: usize,
    scratch: &mut Vec<u8>,
    handler: &mut H,
) -> Result<TrackOutcome, MidiError> {
    let compat = handler.compatibility();
    let chunk_end = src.position().saturating_add(chunk_len);
    let mut st = TrackState::default();
    // Two instantiations: the strict one compiles without any compatibility
    // check (the hot path of every import stays as it was).
    let r = if compat {
        read_track_events::<R, H, true>(src, chunk_len, track, scratch, handler, &mut st)
    } else {
        read_track_events::<R, H, false>(src, chunk_len, track, scratch, handler, &mut st)
    };
    let outcome = match r {
        Ok(o) => o,
        Err(e)
            if compat
                && matches!(
                    e.kind,
                    MidiErrorKind::EventPastChunkEnd | MidiErrorKind::UnexpectedEof
                ) =>
        {
            let here = src.position();
            let start = st.event_start;
            let past_file = src.total_len().is_some_and(|t| chunk_end > t);
            // The chunk is whole (its declared end is where the next chunk
            // starts) unless the file ends first.
            let whole = e.kind == MidiErrorKind::EventPastChunkEnd
                && !past_file
                && match src.skip(chunk_end.saturating_sub(here)) {
                    Ok(()) => true,
                    Err(e2) if e2.kind == MidiErrorKind::UnexpectedEof => false,
                    Err(e2) => return Err(e2),
                };
            if whole {
                handler.warning(ImportWarning::EventCutAtChunkEnd {
                    track,
                    offset: start,
                    dropped: chunk_end - start,
                });
            } else {
                // The file ends inside the chunk: the rest of the file is the
                // cut event.
                let _ = src.skip(u64::MAX);
                let eof = src.position();
                handler.warning(ImportWarning::TrackCutOff {
                    track,
                    offset: eof,
                    dropped: eof.saturating_sub(start),
                    missing: chunk_end.saturating_sub(eof),
                });
            }
            TrackOutcome {
                end_of_track_tick: None,
            }
        }
        Err(e) => return Err(e),
    };
    if st.long_vlq > 0 {
        handler.warning(ImportWarning::LongVlq {
            track,
            count: st.long_vlq,
            first_offset: st.long_first,
            longest: st.long_bytes,
            largest: st.long_largest,
        });
    }
    finish_warnings(handler, track, st.lenient_running, st.system_count);
    Ok(outcome)
}

#[inline]
fn read_track_events<R: Read, H: SmfHandler, const COMPAT: bool>(
    src: &mut ByteSource<R>,
    chunk_len: u64,
    track: usize,
    scratch: &mut Vec<u8>,
    handler: &mut H,
    st: &mut TrackState,
) -> Result<TrackOutcome, MidiError> {
    let compat = COMPAT;
    let start = src.position();
    let chunk_end = start.saturating_add(chunk_len);
    let file_len = src.total_len();
    let mut tick: u64 = 0;
    let mut running: Option<u8> = None;
    let mut after_meta = false;
    let mut lenient_running = 0usize;
    let mut system_count = 0usize;
    let mut next_progress = start + PROGRESS_INTERVAL;
    let extended_keys = handler.extended_keys();

    // Reads one byte that must lie inside the chunk.
    macro_rules! byte {
        () => {{
            if src.position() >= chunk_end {
                return Err(MidiError::at(
                    MidiErrorKind::EventPastChunkEnd,
                    src.position(),
                ));
            }
            src.read_u8()?
        }};
    }
    macro_rules! data_byte {
        () => {{
            let pos = src.position();
            let b = byte!();
            if b >= 0x80 {
                return Err(MidiError::at(MidiErrorKind::StatusInData(b), pos));
            }
            b
        }};
    }
    // The key byte of a Note On / Off after an explicit status: a data byte,
    // or (256-key reading) any byte. A standard read names a byte ≥ 0x80
    // followed by a valid velocity as a non-standard 256-key note instead of
    // calling it a misplaced status byte.
    macro_rules! key_byte {
        () => {{
            let pos = src.position();
            let b = byte!();
            if b >= 0x80 && !extended_keys {
                let velocity = if src.position() < chunk_end {
                    src.peek::<1>()?.map(|v| v[0])
                } else {
                    None
                };
                let kind = match velocity {
                    Some(v) if v < 0x80 => MidiErrorKind::WideKeyNote(b),
                    _ => MidiErrorKind::StatusInData(b),
                };
                return Err(MidiError::at(kind, pos));
            }
            b
        }};
    }
    macro_rules! vlq {
        () => {{
            let pos = src.position();
            let mut v: u64 = 0;
            let mut i = 0u32;
            loop {
                let b = byte!();
                // Only a compatibility reading gets past 4 bytes (28 bits).
                if compat && v > (u64::MAX >> 7) {
                    return Err(MidiError::at(MidiErrorKind::VlqTooLong, pos));
                }
                v = (v << 7) | u64::from(b & 0x7F);
                i += 1;
                if b & 0x80 == 0 {
                    break;
                }
                if i == if compat { COMPAT_VLQ_BYTES } else { 4 } {
                    return Err(MidiError::at(MidiErrorKind::VlqTooLong, pos));
                }
            }
            if compat && i > 4 {
                if st.long_vlq == 0 {
                    st.long_first = pos;
                }
                st.long_vlq += 1;
                st.long_bytes = st.long_bytes.max(i);
                st.long_largest = st.long_largest.max(v);
            }
            v
        }};
    }
    // Reads a payload of `len` bytes into `scratch`, validating the length first.
    macro_rules! payload {
        ($len:expr) => {{
            let len: u64 = $len;
            let pos = src.position();
            if pos.saturating_add(len) > chunk_end {
                return Err(MidiError::at(MidiErrorKind::EventPastChunkEnd, pos));
            }
            if let Some(t) = file_len {
                if pos.saturating_add(len) > t {
                    return Err(MidiError::at(MidiErrorKind::UnexpectedEof, t));
                }
            }
            scratch.clear();
            scratch
                .try_reserve(len as usize)
                .map_err(|_| MidiError::new(MidiErrorKind::OutOfMemory))?;
            scratch.resize(len as usize, 0);
            src.read_exact(&mut scratch[..])?;
        }};
    }

    loop {
        let pos = src.position();
        if compat {
            st.event_start = pos;
        }
        if pos >= next_progress {
            handler.progress(pos)?;
            next_progress = pos + PROGRESS_INTERVAL;
        }
        if pos == chunk_end {
            handler.warning(ImportWarning::MissingEndOfTrack { track });
            st.lenient_running = lenient_running;
            st.system_count = system_count;
            return Ok(TrackOutcome {
                end_of_track_tick: None,
            });
        }
        if pos > chunk_end {
            return Err(MidiError::at(MidiErrorKind::EventPastChunkEnd, pos));
        }
        let delta = vlq!();
        tick = tick
            .checked_add(delta)
            .ok_or_else(|| MidiError::at(MidiErrorKind::TickOverflow, pos))?;
        let status_pos = src.position();
        let b = byte!();
        if b < 0x80 {
            // Running status: `b` is the first data byte.
            let Some(status) = running else {
                return Err(MidiError::at(
                    MidiErrorKind::DataWithoutStatus(b),
                    status_pos,
                ));
            };
            if after_meta {
                lenient_running += 1;
                if COMPAT {
                    st.lenient_running = lenient_running;
                }
            }
            after_meta = false;
            let mut data = [b, 0];
            if channel_data_len(status) == 2 {
                data[1] = data_byte!();
            }
            handler.token(Token::Channel { tick, status, data })?;
            continue;
        }
        match b {
            0x80..=0xEF => {
                running = Some(b);
                after_meta = false;
                let mut data = [if b < 0xA0 { key_byte!() } else { data_byte!() }, 0];
                if channel_data_len(b) == 2 {
                    data[1] = data_byte!();
                }
                handler.token(Token::Channel {
                    tick,
                    status: b,
                    data,
                })?;
            }
            0xFF => {
                let meta_type = byte!();
                let len = vlq!();
                payload!(len);
                after_meta = true;
                if meta_type == meta::END_OF_TRACK {
                    let here = src.position();
                    if here < chunk_end {
                        let past_eof = file_len.is_some_and(|t| chunk_end > t);
                        if past_eof {
                            // Declared length is bogus. Resume at the next chunk
                            // header if one follows the End of Track.
                            handler.warning(ImportWarning::ChunkLengthPastEof { track });
                        } else {
                            let extra = chunk_end - here;
                            src.skip(extra)?;
                            handler.warning(ImportWarning::DataAfterEndOfTrack {
                                track,
                                bytes: extra,
                            });
                        }
                    }
                    st.lenient_running = lenient_running;
                    st.system_count = system_count;
                    return Ok(TrackOutcome {
                        end_of_track_tick: Some(tick),
                    });
                }
                handler.token(Token::Meta {
                    tick,
                    meta_type,
                    payload: &scratch[..],
                })?;
            }
            0xF0 | 0xF7 => {
                let len = vlq!();
                payload!(len);
                after_meta = true;
                let payload = &scratch[..];
                handler.token(if b == 0xF0 {
                    Token::SysEx { tick, payload }
                } else {
                    Token::SysExEscape { tick, payload }
                })?;
            }
            _ => {
                let Some(n) = system_data_len(b) else {
                    return Err(MidiError::at(MidiErrorKind::UndefinedStatus(b), status_pos));
                };
                let mut data = [0u8; 2];
                for d in data.iter_mut().take(n) {
                    *d = data_byte!();
                }
                system_count += 1;
                if COMPAT {
                    st.system_count = system_count;
                }
                handler.token(Token::System {
                    tick,
                    status: b,
                    data,
                })?;
            }
        }
    }
}

fn finish_warnings<H: SmfHandler>(handler: &mut H, track: usize, lenient: usize, system: usize) {
    if lenient > 0 {
        handler.warning(ImportWarning::RunningStatusAcrossMeta {
            track,
            count: lenient,
        });
    }
    if system > 0 {
        handler.warning(ImportWarning::SystemMessages {
            track,
            count: system,
        });
    }
}
