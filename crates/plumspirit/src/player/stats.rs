//! The Player's statistics panel (v0.9 V09-S17, `docs/PLAYER.md` P4 / P10):
//! the Progressor's template engine (`Template<F: Field>`) with the
//! Player's own fields, in the order §054 lists them. Every number comes
//! from the project's analysis index (`Scope::Project`), so it is exact at
//! any position, also right after a seek.

use crate::app::progressor::{Field, NumberFormat, Template};

/// A value the Player's panel can show (`{key}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerField {
    Time,
    Length,
    Fps,
    Nps,
    PeakNps,
    Poly,
    PeakPoly,
    Bpm,
    TotalNotes,
    Passed,
    Progress,
}

impl PlayerField {
    pub const ALL: [PlayerField; 11] = [
        PlayerField::Time,
        PlayerField::Length,
        PlayerField::Fps,
        PlayerField::Nps,
        PlayerField::PeakNps,
        PlayerField::Poly,
        PlayerField::PeakPoly,
        PlayerField::Bpm,
        PlayerField::TotalNotes,
        PlayerField::Passed,
        PlayerField::Progress,
    ];
}

impl Field for PlayerField {
    fn all() -> &'static [PlayerField] {
        &PlayerField::ALL
    }

    fn key(self) -> &'static str {
        match self {
            PlayerField::Time => "time",
            PlayerField::Length => "length",
            PlayerField::Fps => "fps",
            PlayerField::Nps => "nps",
            PlayerField::PeakNps => "peak_nps",
            PlayerField::Poly => "poly",
            PlayerField::PeakPoly => "peak_poly",
            PlayerField::Bpm => "bpm",
            PlayerField::TotalNotes => "total_notes",
            PlayerField::Passed => "passed",
            PlayerField::Progress => "progress",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            PlayerField::Time => "song time at the playhead",
            PlayerField::Length => "length of the exported file (its last note or event)",
            PlayerField::Fps => "frames drawn in the last second while playing (— while paused)",
            PlayerField::Nps => "note starts in the last second of song time",
            PlayerField::PeakNps => "highest NPS from the start of the song to the playhead",
            PlayerField::Poly => "notes sounding at the playhead",
            PlayerField::PeakPoly => "highest polyphony from the start of the song to the playhead",
            PlayerField::Bpm => "tempo at the playhead",
            PlayerField::TotalNotes => "notes in the exported file",
            PlayerField::Passed => "notes that have ended at the playhead",
            PlayerField::Progress => "passed notes as a percentage of all notes",
        }
    }
}

/// The panel's lines (§054's order), as the settings window lists them:
/// (checkbox label, template line).
pub const FIELDS: [(&str, &str); 10] = [
    ("Time", "Time: {time} / {length}"),
    ("FPS", "FPS: {fps}"),
    ("NPS", "NPS: {nps}"),
    ("Peak NPS", "Peak NPS: {peak_nps}"),
    ("Polyphony", "Polyphony: {poly}"),
    ("Peak Polyphony", "Peak Polyphony: {peak_poly}"),
    ("BPM", "BPM: {bpm}"),
    ("Notes", "Notes: {total_notes}"),
    ("Passed", "Passed: {passed}"),
    ("Progress", "Progress: {progress} %"),
];

/// The panel's text when nothing has been changed: §054's nine values, the
/// time with the file's length (Kiva's "Time: 1:04.7 / 4:32.1"); the
/// progress line is offered in the settings.
pub const DEFAULT_TEMPLATE: &str = "Time: {time} / {length}\nFPS: {fps}\nNPS: {nps}\nPeak NPS: {peak_nps}\n\
Polyphony: {poly}\nPeak Polyphony: {peak_poly}\nBPM: {bpm}\nNotes: {total_notes}\nPassed: {passed}";

/// Everything the panel can show, already measured.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerReadout {
    pub time: String,
    /// The exported file's length ("—" before the first snapshot).
    pub length: String,
    /// `None`: not playing (nothing repaints, so there is no frame rate).
    pub fps: Option<u32>,
    pub nps: u64,
    /// `None` until the peak profile is ready.
    pub peak_nps: Option<u64>,
    pub poly: u64,
    pub peak_poly: Option<u64>,
    pub bpm: String,
    pub total: u64,
    pub passed: u64,
}

/// The panel text for `r` (at most `progressor::MAX_LINES` lines).
pub fn render_text(t: &Template<PlayerField>, r: &PlayerReadout, f: NumberFormat) -> String {
    let pending = || "…".to_string();
    let text = t.render(|k| match k {
        PlayerField::Time => r.time.clone(),
        PlayerField::Length => r.length.clone(),
        PlayerField::Fps => r.fps.map_or_else(|| "—".to_string(), |v| v.to_string()),
        PlayerField::Nps => f.count(r.nps),
        PlayerField::PeakNps => r.peak_nps.map_or_else(pending, |v| f.count(v)),
        PlayerField::Poly => f.count(r.poly),
        PlayerField::PeakPoly => r.peak_poly.map_or_else(pending, |v| f.count(v)),
        PlayerField::Bpm => r.bpm.clone(),
        PlayerField::TotalNotes => f.count(r.total),
        PlayerField::Passed => f.count(r.passed),
        PlayerField::Progress => {
            if r.total == 0 {
                "0.0".into()
            } else {
                format!("{:.1}", r.passed as f64 * 100.0 / r.total as f64)
            }
        }
    });
    text.lines()
        .take(crate::app::progressor::MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Frames drawn in the last second (the Player's FPS, §054): the times of
/// the frames drawn while playing, kept for one second.
#[derive(Debug, Default)]
pub struct FrameRate {
    frames: std::collections::VecDeque<std::time::Instant>,
}

impl FrameRate {
    /// A frame drawn at `now` while playing.
    pub fn frame(&mut self, now: std::time::Instant) {
        self.frames.push_back(now);
        self.trim(now);
    }

    /// Frames in the second before `now`.
    pub fn fps(&mut self, now: std::time::Instant) -> u32 {
        self.trim(now);
        self.frames.len() as u32
    }

    /// Paused or stopped: the count starts again.
    pub fn reset(&mut self) {
        self.frames.clear();
    }

    fn trim(&mut self, now: std::time::Instant) {
        let second = std::time::Duration::from_secs(1);
        while self
            .frames
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) >= second)
        {
            self.frames.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn the_default_panel_is_every_field_line_in_the_contracts_order() {
        let lines: Vec<&str> = FIELDS.iter().map(|&(_, l)| l).collect();
        assert_eq!(DEFAULT_TEMPLATE, lines[..9].join("\n"));
        let t = Template::<PlayerField>::parse(DEFAULT_TEMPLATE);
        assert!(t.errors.is_empty(), "{:?}", t.errors);
        // Every field appears once, in PlayerField::ALL's order.
        let mut seen = Vec::new();
        let _ = t.render(|f| {
            seen.push(f);
            String::new()
        });
        let all_but_progress: Vec<PlayerField> = PlayerField::ALL
            .into_iter()
            .filter(|&f| f != PlayerField::Progress)
            .collect();
        assert_eq!(seen, all_but_progress);
        for f in PlayerField::ALL {
            assert_eq!(PlayerField::parse(f.key()), Some(f));
            assert!(!f.describe().is_empty());
        }
        assert!(Template::<PlayerField>::parse("{notes}").errors[0].contains("{notes}"));
    }

    #[test]
    fn the_panel_text_shows_dashes_while_paused_and_dots_until_peaks_arrive() {
        let t = Template::<PlayerField>::parse(DEFAULT_TEMPLATE);
        let mut r = PlayerReadout {
            time: "1:02.500".into(),
            length: "4:32.125".into(),
            fps: None,
            nps: 12_345,
            peak_nps: None,
            poly: 7,
            peak_poly: None,
            bpm: "140".into(),
            total: 40_000_000,
            passed: 1_000,
        };
        let text = render_text(&t, &r, NumberFormat::Grouped);
        assert_eq!(
            text,
            "Time: 1:02.500 / 4:32.125\nFPS: —\nNPS: 12,345\nPeak NPS: …\nPolyphony: 7\n\
             Peak Polyphony: …\nBPM: 140\nNotes: 40,000,000\nPassed: 1,000"
        );
        r.fps = Some(58);
        r.peak_nps = Some(20_000);
        r.peak_poly = Some(9);
        let text = render_text(&t, &r, NumberFormat::Compact);
        assert!(text.contains("FPS: 58\n"), "{text}");
        assert!(text.contains("Peak NPS: 20.0K\n"), "{text}");
        assert!(text.contains("Peak Polyphony: 9\n"), "{text}");
        assert!(text.ends_with("Notes: 40.0M\nPassed: 1.0K"), "{text}");
        let progress = Template::<PlayerField>::parse("Progress: {progress} %");
        assert_eq!(
            render_text(&progress, &r, NumberFormat::Plain),
            "Progress: 0.0 %"
        );
        r.passed = 10_000_000;
        assert_eq!(
            render_text(&progress, &r, NumberFormat::Plain),
            "Progress: 25.0 %"
        );
        // Bounded like the Progressor.
        let long = Template::<PlayerField>::parse(&"{fps}\n".repeat(100));
        assert_eq!(
            render_text(&long, &r, NumberFormat::Plain).lines().count(),
            crate::app::progressor::MAX_LINES
        );
    }

    #[test]
    fn the_frame_rate_counts_the_frames_of_the_last_second() {
        let t0 = Instant::now();
        let mut f = FrameRate::default();
        for k in 0..120 {
            f.frame(t0 + Duration::from_millis(k * 1000 / 60));
        }
        // 1.999 s: the frames at 1.000 s … 1.983 s are less than a second
        // old; at 2.0 s the one at 1.000 s is a second old and left out.
        assert_eq!(f.fps(t0 + Duration::from_millis(1999)), 60);
        assert_eq!(f.fps(t0 + Duration::from_millis(2000)), 59);
        assert_eq!(f.fps(t0 + Duration::from_millis(5000)), 0);
        f.frame(t0 + Duration::from_millis(5001));
        f.reset();
        assert_eq!(f.fps(t0 + Duration::from_millis(5002)), 0);
    }
}
