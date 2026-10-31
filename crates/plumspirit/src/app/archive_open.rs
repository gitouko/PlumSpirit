//! v1.0-Launch L-D02 / L-D04: File → Open on an archive.
//!
//! `start_open` recognises an archive by its content and unpacks it on a
//! worker (`jobs::spawn_open_archive`: the bounded `archive::open_archive`
//! plus the chooser's quick information). No MIDI file in it → an error box
//! with the reasons; one → it opens directly; several → the chooser
//! (`Dialog::ArchiveChooser`): logical path, size, format, tracks and notes,
//! an exact selection or Cancel — never an arbitrary pick.
//!
//! The chosen entry is imported from its spooled copy exactly like a MIDI
//! opened directly (the same bytes, the same import). Its [`ImportOrigin`]
//! names the archive everywhere: the job label, the status line, the window
//! title and Save's folder (`<archive folder>/<entry name>`), Recent Files
//! (the archive, which opens the chooser again). The [`ArchiveSession`] owns
//! the spool and lives while the chooser, an import of one of its files or
//! the 256-key question about one is open; then the spool is deleted.

use super::dialogs::Dialog;
use super::jobs::{self, ImportOrigin, JobKind};
use super::{PlumSpiritApp, StatusKind, files};
use crate::archive::{ArchiveError, Opened, SmfInfo};
use crate::recent::RecentKind;
use plumspirit_engine::model::project::KeyRange;
use std::path::{Path, PathBuf};

/// An unpacked archive whose MIDI files can be opened.
pub struct ArchiveSession {
    /// The archive on disk.
    pub path: PathBuf,
    /// The MIDI files and the spool that holds them.
    pub opened: Opened,
}

impl ArchiveSession {
    /// The origin of a spooled file of this session.
    pub fn origin_of(&self, file: &Path) -> Option<ImportOrigin> {
        let c = self.opened.candidates.iter().find(|c| c.file == file)?;
        let entry = c.chain.last().cloned().unwrap_or_default();
        let file_name = entry.rsplit('/').next().unwrap_or(&entry).to_owned();
        Some(ImportOrigin {
            archive: self.path.clone(),
            logical: c.logical_path(),
            file_name,
        })
    }

    fn holds(&self, file: &Path) -> bool {
        file.starts_with(self.opened.spool.dir())
    }
}

/// One line of the chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChooserRow {
    /// The logical path inside the archive (`songs/a.mid`, `b.tar.gz › b.tar › c.mid`).
    pub logical: String,
    pub size: u64,
    pub info: SmfInfo,
}

impl ChooserRow {
    /// `1.2 MB · Format 1 · 16 tracks · 1,234,567 notes`.
    pub fn details(&self) -> String {
        use plumspirit_engine::midi::group_digits;
        let mut parts = vec![crate::diagnostics::format_bytes(self.size)];
        if let Some(f) = self.info.format {
            parts.push(format!("Format {f}"));
        }
        if let Some(t) = self.info.tracks {
            parts.push(format!(
                "{} track{}",
                group_digits(u64::from(t)),
                if t == 1 { "" } else { "s" }
            ));
        }
        parts.push(match self.info.notes {
            Some(n) => format!("{} note{}", group_digits(n), if n == 1 { "" } else { "s" }),
            None => "notes counted when opened".into(),
        });
        parts.join(" · ")
    }
}

/// The archive entry chooser (several MIDI files in one archive).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveChooser {
    /// The archive's file name.
    pub archive: String,
    pub rows: Vec<ChooserRow>,
    pub selected: usize,
    /// Entries that were skipped and why.
    pub notes: Vec<String>,
}

impl PlumSpiritApp {
    /// The origin of `path` when it is a spooled file of the open archive.
    pub(super) fn archive_origin_for(&self, path: &Path) -> Option<ImportOrigin> {
        self.archive_session.as_ref()?.origin_of(path)
    }

    /// Starts unpacking the archive at `path` (L-D02).
    pub(super) fn start_open_archive(&mut self, path: PathBuf, ctx: &egui::Context) {
        if let Some(dir) = path.parent() {
            self.last_dir = Some(dir.to_path_buf());
        }
        // A new archive replaces the one the chooser came from.
        self.archive_session = None;
        log::info!("opening {} as an archive", path.display());
        let id = self.next_id();
        match jobs::spawn_open_archive(id, path, ctx) {
            Ok(job) => self.job = Some(job),
            Err(e) => self.show_error(
                "Could not open the archive.",
                format!("A worker thread could not be started: {e}"),
            ),
        }
    }

    pub(super) fn finish_open_archive(
        &mut self,
        path: PathBuf,
        result: Result<(Opened, Vec<SmfInfo>), ArchiveError>,
        ctx: &egui::Context,
    ) {
        let name = files::display_name(&path);
        let (opened, infos) = match result {
            Ok(r) => r,
            Err(ArchiveError::Cancelled) => {
                self.set_status("Opening the archive was canceled.", StatusKind::Info);
                return;
            }
            Err(e) => {
                log::error!("archive {} not opened: {e:?}", path.display());
                self.note_recent(&path, RecentKind::Midi, false);
                self.show_error(
                    "Could not open the archive.",
                    format!("{name}\n\n{}", e.user_message()),
                );
                return;
            }
        };
        for n in &opened.notes {
            log::info!("archive {}: {n}", path.display());
        }
        if opened.candidates.is_empty() {
            self.note_recent(&path, RecentKind::Midi, false);
            let mut body = format!("{name}\n\nThere is no MIDI file in this archive.");
            if !opened.notes.is_empty() {
                body.push_str("\n\n");
                body.push_str(&opened.notes.join("\n"));
            }
            self.show_error("No MIDI file was found in the archive.", body);
            return;
        }
        let rows: Vec<ChooserRow> = opened
            .candidates
            .iter()
            .zip(&infos)
            .map(|(c, info)| ChooserRow {
                logical: c.chain[1..].join(" › "),
                size: c.size,
                info: *info,
            })
            .collect();
        let notes = opened.notes.clone();
        let single = rows.len() == 1;
        self.archive_session = Some(ArchiveSession { path, opened });
        if single {
            self.open_archive_entry(0, ctx);
        } else {
            self.dialog = Some(Dialog::ArchiveChooser(Box::new(ArchiveChooser {
                archive: name,
                rows,
                selected: 0,
                notes,
            })));
        }
    }

    /// Opens candidate `i` of the open archive.
    pub(super) fn open_archive_entry(&mut self, i: usize, ctx: &egui::Context) {
        let Some(file) = self
            .archive_session
            .as_ref()
            .and_then(|s| s.opened.candidates.get(i))
            .map(|c| c.file.clone())
        else {
            return;
        };
        self.start_open_with(file, KeyRange::Standard, ctx);
    }

    /// The notes of the open archive (skipped entries), for the status details.
    pub(super) fn archive_notes(&self) -> Vec<String> {
        self.archive_session
            .as_ref()
            .map(|s| s.opened.notes.clone())
            .unwrap_or_default()
    }

    /// Drops the archive session (and deletes its spool) once nothing can
    /// still open one of its files: no chooser, no import of one of them, no
    /// 256-key question about one. Called every frame.
    pub(super) fn release_unused_archive(&mut self) {
        let Some(s) = self.archive_session.as_ref() else {
            return;
        };
        let dialog = match &self.dialog {
            Some(Dialog::ArchiveChooser(_)) => true,
            Some(Dialog::OpenWideKeys { path, .. } | Dialog::OpenCompatible { path, .. }) => {
                s.holds(path)
            }
            _ => false,
        };
        let job = match self.job.as_ref().map(|j| &j.kind) {
            Some(JobKind::Import { path, .. }) => s.holds(path),
            _ => false,
        };
        if !dialog && !job {
            log::info!(
                "archive {} closed; temporary files removed",
                s.path.display()
            );
            self.archive_session = None;
        }
    }
}

/// Whether the file at `path` is an archive (by its first bytes).
pub fn is_archive_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::with_capacity(crate::archive::HEAD);
    if f.take(crate::archive::HEAD as u64)
        .read_to_end(&mut head)
        .is_err()
    {
        return false;
    }
    crate::archive::detect(&head).is_archive()
}

#[cfg(test)]
mod tests {
    use super::super::DocSource;
    use super::super::dialogs::{Dialog, DialogOutcome};
    use super::super::test_support::Harness;
    use crate::archive::tests::sevenz::sz_enc;
    use crate::archive::tests::{gz, midi, tar, ze, zip};
    use plumspirit_engine::model::Bpm;
    use plumspirit_engine::model::PlumDocument;
    use std::path::{Path, PathBuf};

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Dir {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let d = std::env::temp_dir().join(format!(
                "ps-archive-open-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
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

    fn harness() -> Harness {
        Harness::new(PlumDocument::new_project("start".into(), Bpm::DEFAULT, 960))
    }

    fn open(h: &mut Harness, p: &Path) {
        h.app.start_open(p.to_path_buf(), &h.ctx.clone());
        h.wait_jobs();
    }

    fn source(h: &Harness) -> PathBuf {
        match &h.app.open.as_ref().unwrap().source {
            DocSource::Midi(p) => p.clone(),
            other => panic!("not a MIDI source: {other:?}"),
        }
    }

    fn spool_of(h: &Harness) -> Option<PathBuf> {
        h.app
            .archive_session
            .as_ref()
            .map(|s| s.opened.spool.dir().to_path_buf())
    }

    #[test]
    fn one_midi_in_an_archive_opens_directly_under_the_archives_name() {
        let d = Dir::new("one");
        let a = midi(2, 300);
        let direct = d.file("direct.mid", &a);
        let mut h = harness();
        open(&mut h, &direct);
        let want = h.doc().fingerprint();
        let arch = d.file(
            "Pack.zip",
            &zip(&[ze("readme.txt", b"hello", 0), ze("songs/Song.mid", &a, 8)]),
        );
        open(&mut h, &arch);
        assert!(h.app.dialog.is_none(), "{:?}", h.app.dialog);
        assert_eq!(
            h.doc().fingerprint(),
            want,
            "the same document as the direct file"
        );
        assert_eq!(
            source(&h),
            d.0.join("Song.mid"),
            "shown next to the archive"
        );
        assert_eq!(h.doc().project().name, "Song");
        assert!(h.status().starts_with("Opened Song.mid"), "{}", h.status());
        let details = h.app.status.as_ref().unwrap().details.join("\n");
        assert!(
            details.contains("Opened from the archive: Pack.zip › songs/Song.mid"),
            "{details}"
        );
        assert_eq!(h.app.last_dir.as_deref(), Some(d.0.as_path()));
        let recent: Vec<PathBuf> = h
            .app
            .recent
            .entries()
            .iter()
            .map(|e| e.path.clone())
            .collect();
        assert!(
            recent.contains(&crate::recent::canonical(&arch)),
            "{recent:?}"
        );
        assert!(
            !recent
                .iter()
                .any(|p| p.starts_with(std::env::temp_dir().join("plumspirit-archive")))
        );
        assert!(
            h.app.archive_session.is_none(),
            "the spool is gone once the file is open"
        );
    }

    #[test]
    fn several_midis_are_chosen_exactly_and_cancel_removes_the_spool() {
        let d = Dir::new("several");
        let (a, b, c) = (midi(1, 40), midi(2, 60), midi(3, 80));
        let inner = gz(&tar(&[("deep/c.mid", b'0', &c)]));
        let arch = d.file(
            "pack.7z",
            &sz_enc(
                &[
                    ("a.mid", &a),
                    ("notes.txt", b"text"),
                    ("b.mid", &b),
                    ("more.tar.gz", &inner),
                ],
                &[],
                true,
            ),
        );
        let mut h = harness();
        open(&mut h, &arch);
        let Some(Dialog::ArchiveChooser(ch)) = h.app.dialog.clone() else {
            panic!("the chooser: {:?}", h.app.dialog);
        };
        let rows: Vec<(String, String)> = ch
            .rows
            .iter()
            .map(|r| (r.logical.clone(), r.details()))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "a.mid".to_string(),
                    format!("{} bytes · Format 0 · 1 track · 40 notes", a.len())
                ),
                (
                    "b.mid".to_string(),
                    format!("{} bytes · Format 0 · 1 track · 60 notes", b.len())
                ),
                (
                    "more.tar.gz › more.tar › deep/c.mid".to_string(),
                    format!("{} bytes · Format 0 · 1 track · 80 notes", c.len())
                ),
            ]
        );
        assert_eq!(ch.selected, 0);
        assert_eq!(h.doc().project().name, "start", "nothing opened yet");
        // Exactly the chosen one.
        h.dialog_outcome(DialogOutcome::ArchiveOpen(2));
        h.wait_jobs();
        assert_eq!(h.doc().project().name, "c");
        assert_eq!(source(&h), d.0.join("c.mid"));
        let direct = d.file("c.mid", &c);
        let (want, _) = plumspirit_engine::midi::import_file(&direct, Default::default()).unwrap();
        assert_eq!(h.doc().fingerprint(), want.fingerprint());
        assert!(h.app.archive_session.is_none());
        // Cancel: nothing opens, the spool is removed with the next frame.
        open(&mut h, &arch);
        assert!(matches!(h.app.dialog, Some(Dialog::ArchiveChooser(_))));
        let spool = spool_of(&h).expect("a session");
        assert!(spool.is_dir());
        h.dialog_outcome(DialogOutcome::Closed);
        h.app.release_unused_archive();
        assert!(h.app.archive_session.is_none());
        assert!(!spool.exists(), "spool removed on Cancel");
        assert_eq!(h.doc().project().name, "c", "the open document stays");
    }

    #[test]
    fn archives_without_a_midi_or_damaged_ones_explain_why() {
        let d = Dir::new("none");
        let mut h = harness();
        let arch = d.file(
            "docs.zip",
            &zip(&[
                ze("readme.txt", b"hello", 0),
                ze("old.rar", b"Rar!\x1A\x07\x01\x00rest", 0),
            ]),
        );
        open(&mut h, &arch);
        let Some(Dialog::Message {
            title,
            body,
            is_error,
        }) = h.app.dialog.clone()
        else {
            panic!("{:?}", h.app.dialog);
        };
        assert!(is_error);
        assert_eq!(title, "No MIDI file was found in the archive.");
        assert!(
            body.contains("There is no MIDI file in this archive."),
            "{body}"
        );
        assert!(body.contains("old.rar” is a RAR archive"), "{body}");
        h.app.dialog = None;
        let mut bad = zip(&[ze("a.mid", &midi(1, 30), 8)]);
        let n = bad.len();
        bad.truncate(n - 30);
        let p = d.file("cut.zip", &bad);
        open(&mut h, &p);
        let Some(Dialog::Message { title, body, .. }) = h.app.dialog.clone() else {
            panic!("{:?}", h.app.dialog);
        };
        assert_eq!(title, "Could not open the archive.");
        assert!(body.starts_with("cut.zip\n\n"), "{body}");
        assert_eq!(h.doc().project().name, "start", "the open document stays");
        // A plain MIDI and a project still take their own paths.
        let m = d.file("plain.mid", &midi(4, 10));
        open(&mut h, &m);
        assert_eq!(source(&h), m);
    }

    #[test]
    fn a_256_key_file_in_an_archive_keeps_its_origin_when_opened_with_256_keys() {
        let d = Dir::new("wide");
        let trk = [
            0x00, 0x90, 0xC8, 0x64, 0x60, 0x80, 0xC8, 0x00, 0x00, 0xFF, 0x2F, 0x00,
        ];
        let mut wide = b"MThd\0\0\0\x06\0\x00\0\x01\x01\xE0MTrk".to_vec();
        wide.extend_from_slice(&(trk.len() as u32).to_be_bytes());
        wide.extend_from_slice(&trk);
        let arch = d.file("wide.zip", &zip(&[ze("Wide.mid", &wide, 8)]));
        let mut h = harness();
        open(&mut h, &arch);
        let Some(Dialog::OpenWideKeys { path, message, .. }) = h.app.dialog.clone() else {
            panic!("{:?}", h.app.dialog);
        };
        assert!(message.starts_with("Wide.mid\n\n"), "{message}");
        let spool = spool_of(&h).expect("kept for the question");
        assert!(path.starts_with(&spool));
        h.app.release_unused_archive();
        assert!(
            h.app.archive_session.is_some(),
            "kept while the question is open"
        );
        h.dialog_outcome(DialogOutcome::OpenWideKeys(path, false));
        h.wait_jobs();
        assert_eq!(source(&h), d.0.join("Wide.mid"));
        assert_eq!(
            h.doc().project().keys,
            plumspirit_engine::model::project::KeyRange::Extended256
        );
        assert!(h.app.archive_session.is_none() && !spool.exists());
    }

    /// L-D05: a Note On / Off pair whose deltas use 5-byte quantities.
    fn long_vlq_midi() -> Vec<u8> {
        let trk = [
            0x80, 0x80, 0x80, 0x80, 0x00, 0x90, 60, 100, // 5-byte delta 0
            0x80, 0x80, 0x80, 0x83, 0x60, 0x80, 60, 0, // 5-byte delta 480
            0x00, 0xFF, 0x2F, 0x00,
        ];
        let mut m = b"MThd\0\0\0\x06\0\x00\0\x01\x01\xE0MTrk".to_vec();
        m.extend_from_slice(&(trk.len() as u32).to_be_bytes());
        m.extend_from_slice(&trk);
        m
    }

    #[test]
    fn an_out_of_spec_midi_is_offered_in_compatibility_mode_and_lists_its_repairs() {
        let d = Dir::new("compat");
        let p = d.file("Riddim.mid", &long_vlq_midi());
        let mut h = harness();
        open(&mut h, &p);
        let Some(Dialog::OpenCompatible {
            path,
            message,
            keys,
        }) = h.app.dialog.clone()
        else {
            panic!("{:?}", h.app.dialog);
        };
        assert_eq!(
            message,
            "Riddim.mid\n\nA length or delta-time value is longer than MIDI allows at byte 22 (track 1)."
        );
        assert_eq!(h.doc().project().name, "start", "nothing opened silently");
        h.dialog_outcome(DialogOutcome::OpenCompatible(path, keys));
        h.wait_jobs();
        assert_eq!(h.doc().project().name, "Riddim");
        assert_eq!(source(&h), p);
        let st = h.app.status.as_ref().unwrap();
        assert!(
            st.text
                .starts_with("Opened Riddim.mid in Compatibility Mode — "),
            "{}",
            st.text
        );
        assert!(
            st.details.iter().any(|l| l.starts_with(
                "Compatibility Mode, track 1: 2 delta-time or length value(s) used more than the 4 bytes MIDI allows (the first at byte 22"
            )),
            "{:?}",
            st.details
        );
        assert!(
            h.doc().project().source.is_none(),
            "no strict export from it"
        );
        // Cancel opens nothing.
        let mut h = harness();
        open(&mut h, &p);
        assert!(matches!(h.app.dialog, Some(Dialog::OpenCompatible { .. })));
        h.dialog_outcome(DialogOutcome::Closed);
        assert_eq!(h.doc().project().name, "start");
    }

    #[test]
    fn compatibility_mode_reaches_midi_files_inside_archives() {
        let d = Dir::new("compat-zip");
        let arch = d.file(
            "riddims.zip",
            &zip(&[ze("vip/Riddim VIP.mid", &long_vlq_midi(), 8)]),
        );
        let mut h = harness();
        open(&mut h, &arch);
        let Some(Dialog::OpenCompatible {
            path,
            message,
            keys,
        }) = h.app.dialog.clone()
        else {
            panic!("{:?}", h.app.dialog);
        };
        assert!(message.starts_with("Riddim VIP.mid\n\n"), "{message}");
        let spool = spool_of(&h).expect("kept for the question");
        h.app.release_unused_archive();
        assert!(h.app.archive_session.is_some());
        h.dialog_outcome(DialogOutcome::OpenCompatible(path, keys));
        h.wait_jobs();
        assert_eq!(source(&h), d.0.join("Riddim VIP.mid"));
        assert!(h.status().contains("in Compatibility Mode"));
        assert!(h.app.archive_session.is_none() && !spool.exists());
    }
}
