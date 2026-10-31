//! Semantic normalization of a MIDI file, independent of PlumSpirit's document model.
//!
//! Two files are *semantically equivalent* when they have the same header fields,
//! the same tracks, the same non-note events in the same order at the same ticks, the
//! same multiset of note-on events and the same multiset of note terminations (tick,
//! channel, key, velocity, encoding), and the same End-of-Track ticks. Byte-level
//! details such as running status are ignored.
//!
//! This is used as a test oracle for import → export round trips: it only shares the
//! low-level tokenizer with the importer, not the pairing, storage or export code.

use super::error::{ImportWarning, MidiError};
use super::reader::{ByteSource, SmfHandler, Token, read_smf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SemanticTrack {
    /// Non-note events in file order: (tick, canonical bytes).
    pub other: Vec<(u64, Vec<u8>)>,
    /// Note-on events (velocity > 0): (tick, channel, key, velocity), sorted.
    pub note_ons: Vec<(u64, u8, u8, u8)>,
    /// Terminations: (tick, channel, key, velocity, is_note_on_zero), sorted.
    pub note_offs: Vec<(u64, u8, u8, u8, bool)>,
    pub end_of_track: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SemanticFile {
    pub format: u16,
    pub division: u16,
    pub tracks: Vec<SemanticTrack>,
    pub extra_chunks: Vec<([u8; 4], Vec<u8>)>,
}

impl SemanticFile {
    pub fn total_notes(&self) -> usize {
        self.tracks.iter().map(|t| t.note_ons.len()).sum()
    }
}

pub fn semantic_from_bytes(bytes: &[u8]) -> Result<SemanticFile, MidiError> {
    semantic_from_bytes_with(bytes, false)
}

/// [`semantic_from_bytes`]; `extended_keys` reads the v0.9 experimental
/// 256-key file (keys above 127 after an explicit Note On / Off status).
pub fn semantic_from_bytes_with(
    bytes: &[u8],
    extended_keys: bool,
) -> Result<SemanticFile, MidiError> {
    let mut src = ByteSource::new(bytes, Some(bytes.len() as u64));
    let mut h = SemHandler {
        extended_keys,
        ..SemHandler::default()
    };
    read_smf(&mut src, &mut h)?;
    for t in &mut h.file.tracks {
        t.note_ons.sort_unstable();
        t.note_offs.sort_unstable();
    }
    Ok(h.file)
}

#[derive(Default)]
struct SemHandler {
    file: SemanticFile,
    extended_keys: bool,
}

impl SmfHandler for SemHandler {
    fn header(&mut self, format: u16, _declared: u16, division: u16) -> Result<(), MidiError> {
        self.file.format = format;
        self.file.division = division;
        Ok(())
    }

    fn begin_track(&mut self, _index: usize) -> Result<(), MidiError> {
        self.file.tracks.push(SemanticTrack::default());
        Ok(())
    }

    fn token(&mut self, token: Token<'_>) -> Result<(), MidiError> {
        let t = self.file.tracks.last_mut().expect("begin_track called");
        match token {
            Token::Channel { tick, status, data } => {
                let kind = status & 0xF0;
                let ch = status & 0x0F;
                if kind == 0x90 && data[1] > 0 {
                    t.note_ons.push((tick, ch, data[0], data[1]));
                } else if kind == 0x90 {
                    t.note_offs.push((tick, ch, data[0], 0, true));
                } else if kind == 0x80 {
                    t.note_offs.push((tick, ch, data[0], data[1], false));
                } else {
                    let n = crate::model::events::channel_data_len(status);
                    let mut v = vec![status];
                    v.extend_from_slice(&data[..n]);
                    t.other.push((tick, v));
                }
            }
            Token::Meta {
                tick,
                meta_type,
                payload,
            } => {
                let mut v = vec![0xFF, meta_type];
                v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                v.extend_from_slice(payload);
                t.other.push((tick, v));
            }
            Token::SysEx { tick, payload } => {
                let mut v = vec![0xF0];
                v.extend_from_slice(payload);
                t.other.push((tick, v));
            }
            Token::SysExEscape { tick, payload } => {
                let mut v = vec![0xF7];
                v.extend_from_slice(payload);
                t.other.push((tick, v));
            }
            Token::System { tick, status, data } => {
                t.other.push((tick, vec![status, data[0], data[1]]));
            }
        }
        Ok(())
    }

    fn end_track(&mut self, _index: usize, eot: Option<u64>) -> Result<(), MidiError> {
        if let Some(t) = self.file.tracks.last_mut() {
            t.end_of_track = eot;
        }
        Ok(())
    }

    fn extra_chunk(
        &mut self,
        chunk_type: [u8; 4],
        data: Vec<u8>,
        _after: usize,
    ) -> Result<(), MidiError> {
        self.file.extra_chunks.push((chunk_type, data));
        Ok(())
    }

    fn warning(&mut self, _warning: ImportWarning) {}

    fn extended_keys(&self) -> bool {
        self.extended_keys
    }
}
