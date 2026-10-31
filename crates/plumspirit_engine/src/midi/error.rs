//! MIDI import errors and warnings.

use std::fmt;

/// Formats an integer with thousands separators ("18,432").
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Why a MIDI file could not be imported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MidiErrorKind {
    /// The file could not be opened or read.
    Io {
        kind: std::io::ErrorKind,
        message: String,
    },
    /// The file does not start with an `MThd` chunk.
    NotMidi,
    /// The header chunk is shorter than 6 bytes or truncated.
    TruncatedHeader,
    /// Format word other than 0, 1 or 2.
    UnsupportedFormat(u16),
    /// The file ended in the middle of a chunk or event.
    UnexpectedEof,
    /// An event extends beyond the length declared by its track chunk.
    EventPastChunkEnd,
    /// A variable-length quantity used more than four bytes.
    VlqTooLong,
    /// A data byte appeared where a status byte was required and no running status
    /// was active.
    DataWithoutStatus(u8),
    /// A status byte appeared where a data byte was required.
    StatusInData(u8),
    /// A status byte with no defined meaning in a MIDI file.
    UndefinedStatus(u8),
    /// v0.9 §062: a Note On / Off whose key byte is 0x80–0xFF (followed by a
    /// valid velocity) — a non-standard 256-key file (Kiva's format, or
    /// PlumSpirit's experimental export). A standard read refuses it instead
    /// of misreading it; the 256-key import reads it.
    WideKeyNote(u8),
    /// The file contains no track chunks.
    NoTracks,
    /// Absolute tick position exceeded the internal 64-bit range.
    TickOverflow,
    /// A single track holds more than 4 GB of meta/SysEx payload.
    PayloadTooLarge,
    /// Allocation failed while building the document.
    OutOfMemory,
    /// The import was cancelled by the user.
    Cancelled,
}

/// Import error with location context for logs and concise user messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiError {
    pub kind: MidiErrorKind,
    /// Absolute byte offset in the file where the problem was detected.
    pub offset: Option<u64>,
    /// Zero-based track chunk index, when inside a track.
    pub track: Option<usize>,
}

impl MidiError {
    pub fn new(kind: MidiErrorKind) -> Self {
        MidiError {
            kind,
            offset: None,
            track: None,
        }
    }

    pub fn at(kind: MidiErrorKind, offset: u64) -> Self {
        MidiError {
            kind,
            offset: Some(offset),
            track: None,
        }
    }

    pub fn in_track(mut self, track: usize) -> Self {
        self.track.get_or_insert(track);
        self
    }

    pub fn is_cancelled(&self) -> bool {
        self.kind == MidiErrorKind::Cancelled
    }

    /// Whether the file is a non-standard 256-key file that the 256-key
    /// import reads (v0.9 §062).
    pub fn is_wide_key_file(&self) -> bool {
        matches!(self.kind, MidiErrorKind::WideKeyNote(_))
    }

    /// The user-facing explanation (without "Could not open MIDI file.").
    pub fn user_message(&self) -> String {
        let at = self
            .offset
            .map(|o| format!(" at byte {}", group_digits(o)))
            .unwrap_or_default();
        let track = self
            .track
            .map(|t| format!(" (track {})", t + 1))
            .unwrap_or_default();
        match &self.kind {
            MidiErrorKind::Io { message, .. } => format!("The file could not be read: {message}"),
            MidiErrorKind::NotMidi => {
                "This file is not a Standard MIDI File (it does not start with an MThd header)."
                    .into()
            }
            MidiErrorKind::TruncatedHeader => "The MIDI header is incomplete.".into(),
            MidiErrorKind::UnsupportedFormat(f) => {
                format!("The MIDI format {f} is not defined (only 0, 1 and 2 exist).")
            }
            MidiErrorKind::UnexpectedEof => {
                format!("The track data ended unexpectedly{at}{track}.")
            }
            MidiErrorKind::EventPastChunkEnd => {
                format!(
                    "An event runs past the end of its track chunk{at}{track}; the track length is wrong."
                )
            }
            MidiErrorKind::VlqTooLong => {
                format!("A length or delta-time value is longer than MIDI allows{at}{track}.")
            }
            MidiErrorKind::DataWithoutStatus(b) => {
                format!(
                    "A data byte (0x{b:02X}) appeared without a preceding status byte{at}{track}."
                )
            }
            MidiErrorKind::StatusInData(b) => {
                format!(
                    "A status byte (0x{b:02X}) appeared where note/controller data was expected{at}{track}."
                )
            }
            MidiErrorKind::UndefinedStatus(b) => {
                format!("The file uses the undefined status byte 0x{b:02X}{at}{track}.")
            }
            MidiErrorKind::WideKeyNote(b) => {
                format!(
                    "A note at key {b}{at}{track} is above 127, which Standard MIDI cannot hold. This is a non-standard 256-key MIDI file (written for Kiva or by PlumSpirit's experimental 256-key export). Open it as a 256-Key Mode project to read those notes."
                )
            }
            MidiErrorKind::NoTracks => "The file contains no tracks.".into(),
            MidiErrorKind::TickOverflow => {
                format!("Event times exceed the supported range{at}{track}.")
            }
            MidiErrorKind::PayloadTooLarge => {
                format!("A track holds more than 4 GB of text or SysEx data{track}.")
            }
            MidiErrorKind::OutOfMemory => {
                "There is not enough memory to load this MIDI file.".into()
            }
            MidiErrorKind::Cancelled => "Loading was canceled.".into(),
        }
    }
}

impl MidiError {
    /// v1.0-Launch L-D05: a strict reading failed on something the
    /// compatibility reading repairs deterministically (a quantity longer than
    /// four bytes, an event past its chunk's end, a file cut inside a track).
    pub fn is_repairable(&self) -> bool {
        self.track.is_some()
            && matches!(
                self.kind,
                MidiErrorKind::VlqTooLong
                    | MidiErrorKind::EventPastChunkEnd
                    | MidiErrorKind::UnexpectedEof
            )
    }
}

impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for MidiError {}

impl From<std::io::Error> for MidiError {
    fn from(e: std::io::Error) -> Self {
        MidiError::new(MidiErrorKind::Io {
            kind: e.kind(),
            message: e.to_string(),
        })
    }
}

/// Non-fatal conditions found while importing. The document is still usable; the
/// data involved was preserved unless stated otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportWarning {
    /// Note Off events with no matching active note; kept as raw events.
    UnmatchedNoteOff { track: usize, count: usize },
    /// Note On events never terminated before End of Track; kept as raw events.
    UnmatchedNoteOn { track: usize, count: usize },
    /// Track chunk ended without an End-of-Track event (one is written on export).
    MissingEndOfTrack { track: usize },
    /// Bytes after End of Track inside a track chunk were ignored, as MIDI players do.
    DataAfterEndOfTrack { track: usize, bytes: u64 },
    /// The header's track count differs from the number of track chunks found.
    TrackCountMismatch { declared: u16, found: usize },
    /// Chunk that is not `MTrk`; preserved and written back unchanged.
    UnknownChunk { chunk_type: [u8; 4], bytes: u64 },
    /// Non-standard system messages inside a track; preserved.
    SystemMessages { track: usize, count: usize },
    /// Bytes after the last chunk that do not form a complete chunk; ignored.
    TrailingBytes { bytes: u64 },
    /// Header chunk longer than 6 bytes; the extra bytes were ignored.
    LongHeader { extra: u64 },
    /// Format 0 file with more than one track; exported as Format 1.
    Format0WithSeveralTracks { tracks: usize },
    /// PPQN of zero: no musical grid can be derived; ticks are shown instead.
    ZeroPpqn,
    /// SMPTE frame rate other than 24, 25, 29 or 30.
    UnusualSmpteRate { fps: u8 },
    /// A track chunk's declared length ran past the end of the file; the track ended
    /// with End of Track and reading resumed at the next chunk header.
    ChunkLengthPastEof { track: usize },
    /// Running status continued across a meta or SysEx event (not allowed by the
    /// specification, but accepted).
    RunningStatusAcrossMeta { track: usize, count: usize },
    /// Compatibility mode (L-D05): `count` delta-times / lengths used more
    /// than the four bytes MIDI allows (the first at `first_offset`, the
    /// longest `longest` bytes); their exact values were read.
    LongVlq {
        track: usize,
        count: u64,
        first_offset: u64,
        longest: u32,
        largest: u64,
    },
    /// Compatibility mode: the event at `offset` ran past its track chunk's
    /// declared end; its `dropped` bytes were ignored and the track ends there.
    EventCutAtChunkEnd {
        track: usize,
        offset: u64,
        dropped: u64,
    },
    /// Compatibility mode: the file ends at `offset`, inside this track
    /// (`missing` bytes short of its declared length); the incomplete last
    /// event (`dropped` bytes) was ignored and the track ends there.
    TrackCutOff {
        track: usize,
        offset: u64,
        dropped: u64,
        missing: u64,
    },
    /// How tempo / meter / marker events were gathered into the Conductor.
    Conductor(String),
    /// About PlumSpirit structure records (restored or ignored), a full sentence.
    Structure(String),
}

impl ImportWarning {
    pub fn user_message(&self) -> String {
        match self {
            ImportWarning::Structure(m) => m.clone(),
            ImportWarning::UnmatchedNoteOff { track, count } => format!(
                "Track {}: {} Note Off event(s) had no matching note and were kept as raw events.",
                track + 1,
                group_digits(*count as u64)
            ),
            ImportWarning::UnmatchedNoteOn { track, count } => format!(
                "Track {}: {} note(s) were never released and were kept as raw Note On events.",
                track + 1,
                group_digits(*count as u64)
            ),
            ImportWarning::MissingEndOfTrack { track } => format!(
                "Track {} has no End of Track event; one will be written on export.",
                track + 1
            ),
            ImportWarning::DataAfterEndOfTrack { track, bytes } => format!(
                "Track {}: {} byte(s) after End of Track were ignored.",
                track + 1,
                group_digits(*bytes)
            ),
            ImportWarning::TrackCountMismatch { declared, found } => {
                format!("The header declares {declared} track(s) but the file contains {found}.")
            }
            ImportWarning::UnknownChunk { chunk_type, bytes } => format!(
                "An unknown \"{}\" chunk ({} bytes) was kept unchanged.",
                String::from_utf8_lossy(chunk_type),
                group_digits(*bytes)
            ),
            ImportWarning::SystemMessages { track, count } => format!(
                "Track {}: {} non-standard system message(s) were kept.",
                track + 1,
                group_digits(*count as u64)
            ),
            ImportWarning::TrailingBytes { bytes } => {
                format!(
                    "{} trailing byte(s) after the last chunk were ignored.",
                    group_digits(*bytes)
                )
            }
            ImportWarning::LongHeader { extra } => {
                format!("The MIDI header has {extra} extra byte(s), which were ignored.")
            }
            ImportWarning::Format0WithSeveralTracks { tracks } => format!(
                "This Format 0 file contains {tracks} tracks; it will be exported as Format 1."
            ),
            ImportWarning::ZeroPpqn => {
                "The file's timing division is 0; the grid shows plain ticks.".into()
            }
            ImportWarning::UnusualSmpteRate { fps } => {
                format!("Unusual SMPTE frame rate {fps} fps.")
            }
            ImportWarning::ChunkLengthPastEof { track } => format!(
                "Track {}'s declared length runs past the end of the file; its End of Track was used instead.",
                track + 1
            ),
            ImportWarning::RunningStatusAcrossMeta { track, count } => format!(
                "Track {}: running status continued across {} meta/SysEx event(s) (accepted).",
                track + 1,
                group_digits(*count as u64)
            ),
            ImportWarning::Conductor(note) => note.clone(),
            ImportWarning::LongVlq {
                track,
                count,
                first_offset,
                longest,
                largest,
            } => format!(
                "Compatibility Mode, track {}: {} delta-time or length value(s) used more than the 4 bytes MIDI allows (the first at byte {}, the longest {} bytes); their exact values were read (largest {}).",
                track + 1,
                group_digits(*count),
                group_digits(*first_offset),
                longest,
                group_digits(*largest)
            ),
            ImportWarning::EventCutAtChunkEnd {
                track,
                offset,
                dropped,
            } => format!(
                "Compatibility Mode, track {}: the event at byte {} runs past the end of its track chunk; its {} byte(s) were ignored and the track ends there.",
                track + 1,
                group_digits(*offset),
                group_digits(*dropped)
            ),
            ImportWarning::TrackCutOff {
                track,
                offset,
                dropped,
                missing,
            } => format!(
                "Compatibility Mode, track {}: the file ends at byte {}, inside this track ({} byte(s) short of its declared length); the incomplete last event ({} byte(s)) was ignored and the track ends there.",
                track + 1,
                group_digits(*offset),
                group_digits(*missing),
                group_digits(*dropped)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(18432), "18,432");
        assert_eq!(group_digits(1_000_000), "1,000,000");
    }

    #[test]
    fn message_has_offset() {
        let e = MidiError::at(MidiErrorKind::UnexpectedEof, 18432);
        assert_eq!(
            e.user_message(),
            "The track data ended unexpectedly at byte 18,432."
        );
    }
}
