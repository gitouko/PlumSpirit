//! v1.0-Launch L-F06 (contract §011): Onion Skin defaults that survive
//! closing a project and opening a new MIDI file.
//!
//! Ownership: the Onion Skin of a document is a project setting (saved in its
//! `.lup`, outside undo). The **defaults** of a new project or an opened MIDI
//! file (which carries none) are application settings: with "remember" on
//! (the default) they are whatever the user last chose in any project. A
//! `.lup` keeps its own; opening one never changes the defaults.

use super::PlumSpiritApp;
use plumspirit_engine::model::PlumDocument;

/// L-F06: the Onion Skin fields a new document inherits.
pub(super) type OnionSeen = (
    plumspirit_engine::model::project::OnionMode,
    plumspirit_engine::model::project::OnionOrder,
    plumspirit_engine::model::project::DensityBasis,
    u8,
    bool,
);

fn onion_seen(c: &plumspirit_engine::model::project::OnionConfig) -> OnionSeen {
    (c.mode, c.order, c.density, c.opacity, c.per_track)
}

impl PlumSpiritApp {
    /// L-F06 (Flopp: "my onion settings keep getting nuked when i close the
    /// current midi and open a new midi"): the Onion Skin the user chooses in
    /// any project becomes the default of the next new project or opened MIDI
    /// file. What a document brings with it (a `.lup`'s own settings, the
    /// defaults it was opened with) is only observed, never taken as a
    /// choice. Per frame; cheap (five fields).
    pub(super) fn remember_onion(&mut self) {
        let Some(o) = self.open.as_ref() else {
            self.onion_seen = None;
            return;
        };
        let now = onion_seen(&o.doc.project().onion);
        match self.onion_seen {
            Some((g, seen)) if g == o.generation => {
                if seen == now {
                    return;
                }
                self.onion_seen = Some((g, now));
                if self.settings.onion_remember {
                    let (mode, order, density, opacity, per_track) = now;
                    self.settings.onion_default = crate::settings::OnionDefault {
                        mode,
                        density,
                        opacity,
                        per_track,
                    };
                    self.settings.new_project_onion_order = order;
                    // Saved shortly (an opacity drag changes it every frame).
                    self.settings_save_at =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(800));
                }
            }
            _ => self.onion_seen = Some((o.generation, now)),
        }
    }

    /// L-F06: the Onion Skin of a new project or an opened MIDI file (which
    /// carries none): the layer order from Settings and, with "remember" on,
    /// the mode, density, opacity and per-track choice last used. A
    /// Specific mode has no list in a new document: it starts as Previous
    /// and Next (as Toggle Onion Skin does without a list).
    pub(super) fn apply_onion_defaults(&self, doc: &mut PlumDocument) {
        use plumspirit_engine::model::project::OnionMode;
        let o = &mut doc.project_mut().onion;
        o.order = self.settings.new_project_onion_order;
        if self.settings.onion_remember {
            let d = self.settings.onion_default;
            o.mode = if d.mode == OnionMode::Specific {
                OnionMode::PreviousAndNext
            } else {
                d.mode
            };
            o.density = d.density;
            o.opacity = d.opacity;
            o.per_track = d.per_track;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::commands::Command;
    use super::super::test_support::Harness;
    use crate::settings::OnionDefault;
    use plumspirit_engine::model::project::{DensityBasis, OnionMode, OnionOrder};
    use plumspirit_engine::model::{Bpm, PlumDocument};
    use std::path::{Path, PathBuf};

    const SIZE: egui::Vec2 = egui::vec2(1280.0, 800.0);

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Dir {
            let d = std::env::temp_dir().join(format!("ps-onion-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&d).unwrap();
            Dir(d)
        }
        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn open(h: &mut Harness, p: &Path) {
        h.app.start_open(p.to_path_buf(), &h.ctx.clone());
        h.wait_jobs();
        h.app_frame(SIZE, 1.0);
    }

    fn onion(h: &Harness) -> (OnionMode, OnionOrder, DensityBasis, u8, bool) {
        let o = &h.doc().project().onion;
        (o.mode, o.order, o.density, o.opacity, o.per_track)
    }

    /// Flopp: "my onion settings keep getting nuked when i close the current
    /// midi and open a new midi". The Onion Skin chosen in one project is
    /// what the next MIDI file (and the one after a Close Project) opens
    /// with; a `.lup` keeps its own and does not change the defaults; the
    /// defaults survive a restart (the settings file).
    #[test]
    fn the_onion_skin_chosen_in_one_project_is_the_default_of_the_next_midi_file() {
        let d = Dir::new("defaults");
        let a = d.file("a.mid", &crate::archive::tests::midi(3, 200));
        let b = d.file("b.mid", &crate::archive::tests::midi(4, 200));
        let mut h = Harness::new(PlumDocument::new_project("p".into(), Bpm::DEFAULT, 960));
        h.app_frame(SIZE, 1.0);
        assert_eq!(onion(&h).0, OnionMode::Off, "the first default");
        // The user's choices in the first MIDI file.
        open(&mut h, &a);
        assert!(h.command(Command::Onion(OnionMode::AllOthers)));
        assert!(h.command(Command::OnionOrder(OnionOrder::DenseFirst)));
        assert!(h.command(Command::OnionDensity(DensityBasis::WholeTrack)));
        h.app.set_onion_opacity(62);
        h.app_frame(SIZE, 1.0);
        let chosen = onion(&h);
        assert_eq!(
            h.app.settings.onion_default,
            OnionDefault {
                mode: OnionMode::AllOthers,
                density: DensityBasis::WholeTrack,
                opacity: chosen.3,
                per_track: false,
            }
        );
        assert_eq!(
            h.app.settings.new_project_onion_order,
            OnionOrder::DenseFirst
        );
        // Another MIDI file: the same Onion Skin.
        open(&mut h, &b);
        assert_eq!(onion(&h), chosen, "not nuked by opening a new MIDI file");
        assert!(
            !h.app.open.as_ref().unwrap().dirty(),
            "a default is no change"
        );
        // Close Project, then a MIDI file again.
        h.app.request_close_project();
        assert!(h.app.open.is_none());
        h.app_frame(SIZE, 1.0);
        open(&mut h, &a);
        assert_eq!(onion(&h), chosen, "not nuked by Close Project either");
        // A New Project too.
        h.app.create_project("n", "120", "960", false).unwrap();
        h.app_frame(SIZE, 1.0);
        assert_eq!(onion(&h), chosen);
        // A .lup keeps its own; opening it changes no default.
        let mut own = PlumDocument::new_project("own".into(), Bpm::DEFAULT, 960);
        {
            let o = &mut own.project_mut().onion;
            o.mode = OnionMode::Previous;
            o.order = OnionOrder::SparseFirst;
            o.opacity = 20;
        }
        let lup = d.file(
            "own.lup",
            &plumspirit_engine::native::lup::project_to_bytes(&own).unwrap(),
        );
        open(&mut h, &lup);
        for _ in 0..3 {
            h.app_frame(SIZE, 1.0);
        }
        assert_eq!(
            (onion(&h).0, onion(&h).1, onion(&h).3),
            (OnionMode::Previous, OnionOrder::SparseFirst, 20)
        );
        assert_eq!(h.app.settings.onion_default.mode, OnionMode::AllOthers);
        assert_eq!(
            h.app.settings.new_project_onion_order,
            OnionOrder::DenseFirst
        );
        // A restart reads them back.
        let back = crate::settings::parse(&crate::settings::to_json(&h.app.settings));
        assert!(back.problems.is_empty(), "{:?}", back.problems);
        assert_eq!(back.settings.onion_default, h.app.settings.onion_default);
        assert!(back.settings.onion_remember);
        // A Specific list belongs to its project: a new document starts with
        // Previous and Next.
        h.app.settings.onion_default.mode = OnionMode::Specific;
        open(&mut h, &b);
        assert_eq!(onion(&h).0, OnionMode::PreviousAndNext);
        // Remember off: Onion Skin off (with the layer order from Settings).
        h.app.settings.onion_remember = false;
        open(&mut h, &a);
        assert_eq!(onion(&h).0, OnionMode::Off);
        assert_eq!(onion(&h).1, OnionOrder::DenseFirst);
        // ... and choices are no longer remembered.
        assert!(h.command(Command::Onion(OnionMode::Next)));
        h.app_frame(SIZE, 1.0);
        assert_eq!(h.app.settings.onion_default.mode, OnionMode::Specific);
    }
}
