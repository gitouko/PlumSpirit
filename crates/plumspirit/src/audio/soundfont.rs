//! SoundFont (SF2) validation and loading.
//!
//! Loading happens on a background thread, never in the audio callback. Before
//! RustySynth parses a file, PlumSpirit walks its RIFF structure and checks every
//! chunk length against the real file size, so a damaged or hostile header cannot
//! make the loader allocate more memory than the file contains. The file size is
//! also compared with the memory currently available. A panic inside the parser is
//! caught and reported as a damaged file.

use rustysynth::SoundFont;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A SoundFont ready for synthesis.
pub struct LoadedSoundFont {
    pub path: PathBuf,
    pub font: Arc<SoundFont>,
    pub file_bytes: u64,
    /// Bytes of sample data held in memory.
    pub sample_bytes: u64,
    pub presets: usize,
    pub name: String,
}

impl LoadedSoundFont {
    pub fn file_name(&self) -> String {
        crate::app::files::display_name(&self.path)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SoundFontError {
    NotFound,
    Unreadable(String),
    NotSf2,
    /// SF3 (Ogg-compressed samples) and other variants RustySynth cannot play.
    Unsupported(String),
    Damaged(String),
    NoPresets,
    NotEnoughMemory {
        needed: u64,
        available: u64,
    },
}

impl SoundFontError {
    pub fn user_message(&self) -> String {
        match self {
            SoundFontError::NotFound => "The SoundFont file was not found.".into(),
            SoundFontError::Unreadable(e) => {
                format!(
                    "The SoundFont could not be read: {}.",
                    super::engine::sentence(e)
                )
            }
            SoundFontError::NotSf2 => "This file is not an SF2 SoundFont.".into(),
            SoundFontError::Unsupported(what) => format!(
                "This SoundFont uses {what}, which PlumSpirit cannot play. Choose an SF2 file."
            ),
            SoundFontError::Damaged(detail) => {
                format!("The SoundFont file is damaged or incomplete ({detail}).")
            }
            SoundFontError::NoPresets => "The SoundFont contains no instruments.".into(),
            SoundFontError::NotEnoughMemory { needed, available } => format!(
                "Not enough memory to load this SoundFont (it needs about {} MB; about {} MB is free).",
                needed / (1024 * 1024) + 1,
                available / (1024 * 1024)
            ),
        }
    }
}

fn read4<R: Read>(r: &mut R) -> std::io::Result<[u8; 4]> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn read_u32<R: Read>(r: &mut R) -> std::io::Result<u32> {
    Ok(u32::from_le_bytes(read4(r)?))
}

/// Checks the RIFF layout of an SF2 file without reading sample data. Returns
/// (file size, size of the `smpl` sample chunk).
pub fn precheck(path: &Path) -> Result<(u64, u64), SoundFontError> {
    let mut f = File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => SoundFontError::NotFound,
        _ => SoundFontError::Unreadable(e.to_string()),
    })?;
    let len = f
        .metadata()
        .map_err(|e| SoundFontError::Unreadable(e.to_string()))?
        .len();
    precheck_reader(&mut f, len)
}

pub fn precheck_reader<R: Read + Seek>(f: &mut R, len: u64) -> Result<(u64, u64), SoundFontError> {
    let damaged = |d: &str| SoundFontError::Damaged(d.to_owned());
    let io = |_| SoundFontError::Damaged("the file ends early".into());
    if len < 12 {
        return Err(SoundFontError::NotSf2);
    }
    let riff = read4(f).map_err(io)?;
    let riff_len = u64::from(read_u32(f).map_err(io)?);
    let form = read4(f).map_err(io)?;
    if &riff != b"RIFF" || &form != b"sfbk" {
        return Err(SoundFontError::NotSf2);
    }
    if riff_len + 8 > len {
        return Err(damaged("the RIFF size exceeds the file"));
    }
    let end = 8 + riff_len;
    let mut pos = 12u64;
    let mut smpl = None;
    let mut saw_pdta = false;
    while pos + 8 <= end {
        f.seek(SeekFrom::Start(pos)).map_err(io)?;
        let id = read4(f).map_err(io)?;
        let size = u64::from(read_u32(f).map_err(io)?);
        let body = pos + 8;
        if body + size > end {
            return Err(damaged("a chunk extends past the end of the file"));
        }
        if &id == b"LIST" {
            if size < 4 {
                return Err(damaged("an empty chunk list"));
            }
            let list_type = read4(f).map_err(io)?;
            let mut sub = body + 4;
            let list_end = body + size;
            while sub + 8 <= list_end {
                f.seek(SeekFrom::Start(sub)).map_err(io)?;
                let sid = read4(f).map_err(io)?;
                let ssize = u64::from(read_u32(f).map_err(io)?);
                if sub + 8 + ssize > list_end {
                    return Err(damaged("a sub-chunk extends past its list"));
                }
                if &list_type == b"sdta" && &sid == b"smpl" {
                    if ssize >= 4 {
                        let mut magic = [0u8; 4];
                        f.read_exact(&mut magic).map_err(io)?;
                        if &magic == b"OggS" {
                            return Err(SoundFontError::Unsupported(
                                "compressed samples (SF3)".into(),
                            ));
                        }
                    }
                    smpl = Some(ssize);
                }
                // Sub-chunks are word aligned.
                sub += 8 + ssize + (ssize & 1);
            }
            if &list_type == b"pdta" {
                saw_pdta = true;
            }
        }
        pos = body + size + (size & 1);
    }
    let Some(smpl) = smpl else {
        return Err(damaged("no sample data"));
    };
    if !saw_pdta {
        return Err(damaged("no instrument data"));
    }
    Ok((len, smpl))
}

/// Loads and validates a SoundFont. Call from a background thread.
pub fn load(path: &Path) -> Result<LoadedSoundFont, SoundFontError> {
    let (file_bytes, sample_bytes) = precheck(path)?;
    // RustySynth keeps the samples (16-bit) in memory; leave room for the rest of
    // the application.
    if let Some(avail) = crate::platform::available_memory_bytes() {
        let needed = sample_bytes + sample_bytes / 8 + 16 * 1024 * 1024;
        if needed > avail / 10 * 9 {
            return Err(SoundFontError::NotEnoughMemory {
                needed,
                available: avail,
            });
        }
    }
    let file = File::open(path).map_err(|e| SoundFontError::Unreadable(e.to_string()))?;
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let parsed =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| SoundFont::new(&mut reader)));
    let font = match parsed {
        Ok(Ok(f)) => f,
        Ok(Err(e)) => {
            let msg = e.to_string();
            return Err(if msg.contains("SoundFont3") {
                SoundFontError::Unsupported("compressed samples (SF3)".into())
            } else {
                SoundFontError::Damaged(msg)
            });
        }
        Err(_) => {
            return Err(SoundFontError::Damaged(
                "the file could not be parsed".into(),
            ));
        }
    };
    let presets = font.get_presets().len();
    if presets == 0 {
        return Err(SoundFontError::NoPresets);
    }
    let name = font.get_info().get_bank_name().trim().to_owned();
    let sample_bytes = font.get_wave_data().len() as u64 * 2;
    Ok(LoadedSoundFont {
        path: path.to_path_buf(),
        font: Arc::new(font),
        file_bytes,
        sample_bytes,
        presets,
        name,
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Writes a minimal valid SF2: one preset (bank 0, program 0) with one
    /// instrument playing a looped sine sample over the whole key range. Used by
    /// the audio tests; not a musical SoundFont.
    pub fn tiny_sf2() -> Vec<u8> {
        fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
            let mut v = id.to_vec();
            v.extend((data.len() as u32).to_le_bytes());
            v.extend(data);
            if data.len() % 2 == 1 {
                v.push(0);
            }
            v
        }
        fn list(kind: &[u8; 4], parts: &[Vec<u8>]) -> Vec<u8> {
            let mut body = kind.to_vec();
            for p in parts {
                body.extend(p);
            }
            chunk(b"LIST", &body)
        }
        let name20 = |s: &str| {
            let mut b = s.as_bytes().to_vec();
            b.resize(20, 0);
            b
        };
        // 1 kHz-ish sine at 22,050 Hz, 441 samples (exactly 20 periods of 1102.5 Hz?)
        // Use 100 samples per period so the loop is seamless: 220.5 Hz root.
        let n = 2000usize;
        let mut pcm = Vec::with_capacity(n * 2 + 92);
        for i in 0..n {
            let v = ((i as f64 / 100.0) * std::f64::consts::TAU).sin() * 20_000.0;
            pcm.extend((v as i16).to_le_bytes());
        }
        pcm.extend(vec![0u8; 46 * 2]); // 46 zero samples after the sample (SF2 rule)
        let info = list(
            b"INFO",
            &[
                chunk(b"ifil", &[2, 0, 1, 0]),
                chunk(b"isng", b"EMU8000\0"),
                chunk(b"INAM", b"PlumSpirit Test\0"),
            ],
        );
        let sdta = list(b"sdta", &[chunk(b"smpl", &pcm)]);
        // generators: (oper u16, amount u16)
        let generator = |op: u16, amt: u16| {
            let mut v = op.to_le_bytes().to_vec();
            v.extend(amt.to_le_bytes());
            v
        };
        let mut phdr = Vec::new();
        phdr.extend(name20("Sine"));
        phdr.extend(0u16.to_le_bytes()); // preset
        phdr.extend(0u16.to_le_bytes()); // bank
        phdr.extend(0u16.to_le_bytes()); // bag index
        phdr.extend([0u8; 12]);
        phdr.extend(name20("EOP"));
        phdr.extend(0u16.to_le_bytes());
        phdr.extend(0u16.to_le_bytes());
        phdr.extend(1u16.to_le_bytes());
        phdr.extend([0u8; 12]);
        let mut pbag = Vec::new();
        pbag.extend(0u16.to_le_bytes());
        pbag.extend(0u16.to_le_bytes());
        pbag.extend(1u16.to_le_bytes()); // terminal
        pbag.extend(0u16.to_le_bytes());
        let pmod = vec![0u8; 10];
        let mut pgen = generator(41, 0); // instrument 0
        pgen.extend(generator(0, 0)); // terminal
        let mut inst = name20("SineInst");
        inst.extend(0u16.to_le_bytes());
        inst.extend(name20("EOI"));
        inst.extend(1u16.to_le_bytes());
        let mut ibag = Vec::new();
        ibag.extend(0u16.to_le_bytes());
        ibag.extend(0u16.to_le_bytes());
        ibag.extend(3u16.to_le_bytes());
        ibag.extend(0u16.to_le_bytes());
        let imod = vec![0u8; 10];
        let mut igen = generator(54, 1); // sampleModes: loop continuously
        igen.extend(generator(58, 57)); // overridingRootKey: A3 (220 Hz)
        igen.extend(generator(53, 0)); // sampleID 0 (must be last)
        igen.extend(generator(0, 0)); // terminal
        let mut shdr = name20("Sine");
        shdr.extend(0u32.to_le_bytes()); // start
        shdr.extend((n as u32).to_le_bytes()); // end
        shdr.extend(100u32.to_le_bytes()); // loop start
        shdr.extend(1900u32.to_le_bytes()); // loop end
        shdr.extend(22_050u32.to_le_bytes());
        shdr.push(57); // original pitch
        shdr.push(0); // correction
        shdr.extend(0u16.to_le_bytes()); // link
        shdr.extend(1u16.to_le_bytes()); // mono
        shdr.extend(name20("EOS"));
        shdr.extend([0u8; 26]);
        let pdta = list(
            b"pdta",
            &[
                chunk(b"phdr", &phdr),
                chunk(b"pbag", &pbag),
                chunk(b"pmod", &pmod),
                chunk(b"pgen", &pgen),
                chunk(b"inst", &inst),
                chunk(b"ibag", &ibag),
                chunk(b"imod", &imod),
                chunk(b"igen", &igen),
                chunk(b"shdr", &shdr),
            ],
        );
        let mut body = b"sfbk".to_vec();
        body.extend(info);
        body.extend(sdta);
        body.extend(pdta);
        chunk(b"RIFF", &body)
    }

    fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("plumspirit-sf-{}-{name}", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn valid_sf2_loads() {
        let p = write_temp("ok.sf2", &tiny_sf2());
        let sf = load(&p).unwrap_or_else(|e| panic!("{}", e.user_message()));
        assert_eq!(sf.presets, 1);
        assert_eq!(sf.name, "PlumSpirit Test");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn invalid_files_are_reported() {
        assert_eq!(
            load(Path::new("/definitely/missing.sf2")).err(),
            Some(SoundFontError::NotFound)
        );
        let p = write_temp("text.sf2", b"hello world, not a soundfont");
        assert_eq!(load(&p).err(), Some(SoundFontError::NotSf2));
        let good = tiny_sf2();
        // Truncated: header sizes exceed the file.
        let t = write_temp("trunc.sf2", &good[..good.len() / 2]);
        assert!(matches!(load(&t), Err(SoundFontError::Damaged(_))));
        // A lying sample-chunk length is caught before any allocation.
        let mut bad = good.clone();
        let at = bad.windows(4).position(|w| w == b"smpl").unwrap();
        bad[at + 4..at + 8].copy_from_slice(&0x7FFF_FFF0u32.to_le_bytes());
        let b = write_temp("lie.sf2", &bad);
        assert!(matches!(load(&b), Err(SoundFontError::Damaged(_))));
        // SF3 (Ogg) samples are refused clearly.
        let mut ogg = good.clone();
        ogg[at + 8..at + 12].copy_from_slice(b"OggS");
        let o = write_temp("sf3.sf2", &ogg);
        assert!(matches!(load(&o), Err(SoundFontError::Unsupported(_))));
        // Random corruption never panics.
        for i in (0..good.len()).step_by(37) {
            let mut c = good.clone();
            c[i] ^= 0x5A;
            let f = write_temp("fuzz.sf2", &c);
            let _ = load(&f);
        }
        for f in [p, t, b, o] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Writes the test SoundFont to `$PLUMSPIRIT_TINY_SF2_OUT` (a tiny sine-wave SF2
    /// for checking an audio device without a real SoundFont):
    /// `PLUMSPIRIT_TINY_SF2_OUT=tiny.sf2 cargo test -p plumspirit write_tiny_sf2 -- --ignored`
    #[test]
    #[ignore = "tool: writes a file"]
    fn write_tiny_sf2() {
        if let Some(out) = std::env::var_os("PLUMSPIRIT_TINY_SF2_OUT") {
            std::fs::write(&out, tiny_sf2()).expect("write");
            assert!(load(std::path::Path::new(&out)).is_ok());
        }
    }
}
