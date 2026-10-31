//! Music theory helpers shared by the tools: scales, scale snapping, scale
//! degrees, interval names, chord templates and chord-name / Roman-numeral
//! parsing. Pitch classes are 0 = C … 11 = B.

/// Names of the twelve pitch classes.
pub const KEY_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Built-in scales: (name, semitone steps from the root).
pub const SCALES: &[(&str, &[u8])] = &[
    ("Major (Ionian)", &[0, 2, 4, 5, 7, 9, 11]),
    ("Natural minor (Aeolian)", &[0, 2, 3, 5, 7, 8, 10]),
    ("Harmonic minor", &[0, 2, 3, 5, 7, 8, 11]),
    ("Melodic minor", &[0, 2, 3, 5, 7, 9, 11]),
    ("Dorian", &[0, 2, 3, 5, 7, 9, 10]),
    ("Phrygian", &[0, 1, 3, 5, 7, 8, 10]),
    ("Lydian", &[0, 2, 4, 6, 7, 9, 11]),
    ("Mixolydian", &[0, 2, 4, 5, 7, 9, 10]),
    ("Locrian", &[0, 1, 3, 5, 6, 8, 10]),
    ("Major pentatonic", &[0, 2, 4, 7, 9]),
    ("Minor pentatonic", &[0, 3, 5, 7, 10]),
    ("Blues", &[0, 3, 5, 6, 7, 10]),
    ("Whole tone", &[0, 2, 4, 6, 8, 10]),
    ("Diminished (half-whole)", &[0, 1, 3, 4, 6, 7, 9, 10]),
    ("Diminished (whole-half)", &[0, 2, 3, 5, 6, 8, 9, 11]),
    ("Phrygian dominant", &[0, 1, 4, 5, 7, 8, 10]),
    ("Hungarian minor", &[0, 2, 3, 6, 7, 8, 11]),
    ("Double harmonic", &[0, 1, 4, 5, 7, 8, 11]),
    ("Japanese (In)", &[0, 1, 5, 7, 8]),
    ("Chromatic", &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]),
];

/// Names for parameter menus.
pub fn scale_names() -> &'static [&'static str] {
    const N: [&str; 20] = [
        "Major (Ionian)",
        "Natural minor (Aeolian)",
        "Harmonic minor",
        "Melodic minor",
        "Dorian",
        "Phrygian",
        "Lydian",
        "Mixolydian",
        "Locrian",
        "Major pentatonic",
        "Minor pentatonic",
        "Blues",
        "Whole tone",
        "Diminished (half-whole)",
        "Diminished (whole-half)",
        "Phrygian dominant",
        "Hungarian minor",
        "Double harmonic",
        "Japanese (In)",
        "Chromatic",
    ];
    &N
}

/// A scale on a root: the set of allowed pitch classes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scale {
    pub root: u8,
    /// Steps from the root, ascending, starting with 0.
    pub steps: Vec<u8>,
}

impl Scale {
    pub fn new(root: usize, scale: usize) -> Scale {
        let s = SCALES.get(scale).unwrap_or(&SCALES[0]);
        Scale {
            root: (root % 12) as u8,
            steps: s.1.to_vec(),
        }
    }

    /// From a 12-bit pitch-class mask (bit = pitch class), rooted at `root`.
    pub fn from_mask(root: u8, mask: u16) -> Scale {
        let root = root % 12;
        let mut steps: Vec<u8> = (0..12u8)
            .filter(|i| mask & (1 << ((root + i) % 12)) != 0)
            .collect();
        if steps.is_empty() {
            steps.push(0);
        }
        Scale { root, steps }
    }

    /// 12-bit mask of allowed pitch classes.
    pub fn mask(&self) -> u16 {
        self.steps
            .iter()
            .fold(0u16, |m, s| m | 1 << ((self.root + s) % 12))
    }

    #[inline]
    pub fn contains(&self, pitch: i64) -> bool {
        self.mask() & (1 << pitch.rem_euclid(12)) != 0
    }

    /// Degree (0-based, any octave: degree 7 = root an octave up) and the
    /// semitones the pitch lies above that degree (0 when in the scale).
    pub fn degree_of(&self, pitch: i64) -> (i64, i64) {
        let n = self.steps.len() as i64;
        let rel = pitch - i64::from(self.root);
        let oct = rel.div_euclid(12);
        let pc = rel.rem_euclid(12) as u8;
        let mut d = 0;
        for (i, s) in self.steps.iter().enumerate() {
            if *s <= pc {
                d = i;
            }
        }
        (oct * n + d as i64, i64::from(pc - self.steps[d]))
    }

    /// The pitch of a degree (0-based; may be negative or beyond one octave).
    pub fn pitch_of(&self, degree: i64) -> i64 {
        let n = self.steps.len() as i64;
        let oct = degree.div_euclid(n);
        let d = degree.rem_euclid(n) as usize;
        i64::from(self.root) + oct * 12 + i64::from(self.steps[d])
    }

    /// Nearest allowed pitch by direction preference.
    pub fn snap(&self, pitch: i64, mode: SnapMode) -> i64 {
        if self.contains(pitch) {
            return pitch;
        }
        let mut up = pitch;
        while !self.contains(up) {
            up += 1;
        }
        let mut down = pitch;
        while !self.contains(down) {
            down -= 1;
        }
        match mode {
            SnapMode::Up => up,
            SnapMode::Down => down,
            SnapMode::Nearest => {
                if up - pitch < pitch - down {
                    up
                } else if up - pitch > pitch - down {
                    down
                } else {
                    // Tie: alternate by pitch parity (deterministic).
                    if pitch.rem_euclid(2) == 0 { up } else { down }
                }
            }
            SnapMode::Alternate(i) => {
                if i % 2 == 0 {
                    up
                } else {
                    down
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapMode {
    Up,
    Down,
    Nearest,
    /// Alternates up / down by the given counter.
    Alternate(u64),
}

/// Short interval names by semitone class.
const SHORT: [&str; 12] = [
    "P1", "m2", "M2", "m3", "M3", "P4", "TT", "P5", "m6", "M6", "m7", "M7",
];
const LONG: [&str; 12] = [
    "unison",
    "minor 2nd",
    "major 2nd",
    "minor 3rd",
    "major 3rd",
    "perfect 4th",
    "tritone",
    "perfect 5th",
    "minor 6th",
    "major 6th",
    "minor 7th",
    "major 7th",
];

/// "+7 · P5 (perfect 5th)", "−12 · P8 (octave)", "+16 · P8 + M3".
pub fn interval_label(semitones: i64) -> String {
    let a = semitones.unsigned_abs() as usize;
    let sign = if semitones > 0 {
        "+"
    } else if semitones < 0 {
        "−"
    } else {
        " "
    };
    let oct = a / 12;
    let rest = a % 12;
    let name = match (oct, rest) {
        (0, r) => format!("{} ({})", SHORT[r], LONG[r]),
        (1, 0) => "P8 (octave)".to_string(),
        (o, 0) => format!("{o} octaves"),
        (1, r) => format!("P8 + {}", SHORT[r]),
        (o, r) => format!("{o}×P8 + {}", SHORT[r]),
    };
    format!("{sign}{a} · {name}")
}

/// Chord templates: (name, symbol suffix, intervals from the root).
pub const CHORDS: &[(&str, &str, &[u8])] = &[
    ("Major", "", &[0, 4, 7]),
    ("Minor", "m", &[0, 3, 7]),
    ("Diminished", "dim", &[0, 3, 6]),
    ("Augmented", "aug", &[0, 4, 8]),
    ("Sus2", "sus2", &[0, 2, 7]),
    ("Sus4", "sus4", &[0, 5, 7]),
    ("Power", "5", &[0, 7, 12]),
    ("Dominant 7", "7", &[0, 4, 7, 10]),
    ("Major 7", "maj7", &[0, 4, 7, 11]),
    ("Minor 7", "m7", &[0, 3, 7, 10]),
    ("Minor major 7", "mMaj7", &[0, 3, 7, 11]),
    ("Half-diminished", "m7b5", &[0, 3, 6, 10]),
    ("Diminished 7", "dim7", &[0, 3, 6, 9]),
    ("Add 9", "add9", &[0, 4, 7, 14]),
    ("Major 6", "6", &[0, 4, 7, 9]),
    ("Minor 6", "m6", &[0, 3, 7, 9]),
    ("Dominant 9", "9", &[0, 4, 7, 10, 14]),
    ("Major 9", "maj9", &[0, 4, 7, 11, 14]),
    ("Minor 9", "m9", &[0, 3, 7, 10, 14]),
    ("Dominant 11", "11", &[0, 4, 7, 10, 14, 17]),
    ("Dominant 13", "13", &[0, 4, 7, 10, 14, 21]),
];

pub fn chord_names() -> Vec<&'static str> {
    CHORDS.iter().map(|c| c.0).collect()
}

/// A parsed chord: root pitch class + intervals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChordShape {
    pub root: u8,
    pub intervals: Vec<u8>,
    /// Optional slash bass pitch class.
    pub bass: Option<u8>,
}

impl ChordShape {
    pub fn name(&self) -> String {
        let suffix = CHORDS
            .iter()
            .find(|c| c.2 == self.intervals.as_slice())
            .map(|c| c.1)
            .unwrap_or("?");
        let mut s = format!("{}{}", KEY_NAMES[usize::from(self.root % 12)], suffix);
        if let Some(b) = self.bass {
            s.push('/');
            s.push_str(KEY_NAMES[usize::from(b % 12)]);
        }
        s
    }

    /// Pitch classes of the chord tones.
    pub fn pitch_classes(&self) -> u16 {
        self.intervals
            .iter()
            .fold(0u16, |m, i| m | 1 << ((self.root + i) % 12))
    }
}

fn parse_root(s: &str) -> Option<(u8, &str)> {
    let mut chars = s.chars();
    let letter = chars.next()?;
    let base: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest = &s[1..];
    let (acc, rest) = if let Some(r) = rest.strip_prefix('#') {
        (1, r)
    } else if let Some(r) = rest.strip_prefix('b') {
        (-1, r)
    } else {
        (0, rest)
    };
    Some((((base + acc).rem_euclid(12)) as u8, rest))
}

/// Parses an absolute chord name: `C`, `Am`, `F#m7`, `Bbmaj7`, `G7/B`, `Esus4`.
pub fn parse_chord(name: &str) -> Option<ChordShape> {
    let name = name.trim();
    let (main, bass) = match name.split_once('/') {
        Some((m, b)) => (m, Some(parse_root(b.trim())?.0)),
        None => (name, None),
    };
    let (root, suffix) = parse_root(main)?;
    let suffix = match suffix {
        "min" | "-" => "m",
        "M7" | "Δ7" | "Maj7" => "maj7",
        "°" => "dim",
        "+" => "aug",
        s => s,
    };
    let c = CHORDS.iter().find(|c| c.1 == suffix)?;
    Some(ChordShape {
        root,
        intervals: c.2.to_vec(),
        bass,
    })
}

/// Parses a Roman numeral (`I`, `vi`, `V7`, `ii°`, `bVII`, `iv`) in `scale`:
/// upper case = major triad, lower case = minor, `°` = diminished, a trailing
/// `7` adds the scale's seventh quality (dominant on V, major 7 on I/IV, minor 7
/// on minor numerals).
pub fn parse_roman(token: &str, scale: &Scale) -> Option<ChordShape> {
    let t = token.trim();
    let (flat, t) = if let Some(r) = t.strip_prefix('b') {
        (-1i64, r)
    } else if let Some(r) = t.strip_prefix('#') {
        (1, r)
    } else {
        (0, t)
    };
    let numerals = [
        ("vii", 7),
        ("iii", 3),
        ("vi", 6),
        ("iv", 4),
        ("v", 5),
        ("ii", 2),
        ("i", 1),
    ];
    let lower = t.to_ascii_lowercase();
    let (num, len) = numerals
        .iter()
        .find(|(n, _)| lower.starts_with(n))
        .map(|(n, d)| (*d, n.len()))?;
    let upper = t[..len].chars().all(|c| c.is_ascii_uppercase());
    let rest = &t[len..];
    let (dim, rest) = match rest.strip_prefix('°').or_else(|| rest.strip_prefix("dim")) {
        Some(r) => (true, r),
        None => (false, rest),
    };
    let seventh = match rest {
        "" => false,
        "7" => true,
        _ => return None,
    };
    let root = (scale.pitch_of(num - 1) + flat).rem_euclid(12) as u8;
    let intervals: Vec<u8> = match (upper, dim, seventh) {
        (_, true, false) => vec![0, 3, 6],
        (_, true, true) => vec![0, 3, 6, 9],
        (true, false, false) => vec![0, 4, 7],
        (false, false, false) => vec![0, 3, 7],
        (true, false, true) => {
            if num == 5 {
                vec![0, 4, 7, 10]
            } else {
                vec![0, 4, 7, 11]
            }
        }
        (false, false, true) => vec![0, 3, 7, 10],
    };
    Some(ChordShape {
        root,
        intervals,
        bass: None,
    })
}

/// Roman numeral of a chord in a scale (for the "relative names" display).
pub fn roman_name(chord: &ChordShape, scale: &Scale) -> String {
    const R: [&str; 7] = ["I", "II", "III", "IV", "V", "VI", "VII"];
    let (deg, off) = scale.degree_of(i64::from(chord.root));
    let d = deg.rem_euclid(scale.steps.len() as i64) as usize;
    let minor = chord.intervals.get(1) == Some(&3);
    let dim = chord.intervals.get(2) == Some(&6);
    let base = if d < 7 { R[d] } else { "?" };
    let mut s = String::new();
    if off > 0 {
        s.push('#');
    }
    if minor {
        s.push_str(&base.to_ascii_lowercase());
    } else {
        s.push_str(base);
    }
    if dim {
        s.push('°');
    }
    if chord.intervals.len() >= 4 {
        s.push('7');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_snap_and_count_degrees() {
        let c = Scale::new(0, 0);
        assert!(c.contains(60) && !c.contains(61));
        assert_eq!(c.snap(61, SnapMode::Up), 62);
        assert_eq!(c.snap(61, SnapMode::Down), 60);
        assert_eq!(c.degree_of(60), (35, 0));
        assert_eq!(c.pitch_of(35), 60);
        assert_eq!(c.pitch_of(36), 62);
        assert_eq!(c.degree_of(61), (35, 1));
        let m = Scale::from_mask(9, c.mask());
        assert_eq!(m.steps, vec![0, 2, 3, 5, 7, 8, 10]);
    }

    #[test]
    fn chord_names_and_roman_numerals_parse() {
        assert_eq!(parse_chord("Am").unwrap().intervals, vec![0, 3, 7]);
        assert_eq!(parse_chord("F#m7").unwrap().root, 6);
        assert_eq!(parse_chord("Bbmaj7").unwrap().root, 10);
        assert_eq!(parse_chord("G7/B").unwrap().bass, Some(11));
        assert!(parse_chord("H").is_none());
        let c = Scale::new(0, 0);
        let vi = parse_roman("vi", &c).unwrap();
        assert_eq!((vi.root, vi.intervals.clone()), (9, vec![0, 3, 7]));
        assert_eq!(parse_roman("V7", &c).unwrap().intervals, vec![0, 4, 7, 10]);
        assert_eq!(parse_roman("vii°", &c).unwrap().intervals, vec![0, 3, 6]);
        assert_eq!(parse_roman("bVII", &c).unwrap().root, 10);
        assert_eq!(roman_name(&vi, &c), "vi");
        assert_eq!(interval_label(7), "+7 · P5 (perfect 5th)");
        assert_eq!(interval_label(-12), "−12 · P8 (octave)");
        assert_eq!(interval_label(16), "+16 · P8 + M3");
    }
}
