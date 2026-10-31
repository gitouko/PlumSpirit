// Semantic hashes of a document (V09-S06, contract §040 / §241): one hash per
// musical category, written by the v0.8 generator next to each fixture
// (`make_v08_fixtures.rs.txt`, run inside PlumSpirit v0.8 = 058a094) and
// recomputed by v0.9 (`tests/v09_clip_migration.rs`). `include!`d by both, so
// it uses only API that exists unchanged in v0.8 and v0.9; the includer
// imports `PlumDocument`.

/// FNV-1a, 64 bit.
struct SemHash(u64);

impl SemHash {
    fn new() -> SemHash {
        SemHash(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn u(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    fn text(&mut self, s: &str) {
        self.u(s.len() as u64);
        self.bytes(s.as_bytes());
    }
}

/// `(category, hash)` in a fixed order: notes, channels, subchannels, groups,
/// events (CC, pitch bend, SysEx, meta …), tempo (the Conductor: tempo points
/// and ramps, signatures, markers / chapters, Master Volume), structure
/// (track order, ids, parents, names, properties, Mute / Solo, folders), and
/// the document's own `fingerprint` / `state_fingerprint`.
fn semantic_hashes(d: &PlumDocument) -> Vec<(&'static str, u64)> {
    let mut notes = SemHash::new();
    let mut channels = SemHash::new();
    let mut subs = SemHash::new();
    let mut groups = SemHash::new();
    let mut events = SemHash::new();
    let mut structure = SemHash::new();
    structure.u(d.track_count() as u64);
    structure.u(u64::from(d.next_track_id()));
    for t in d.tracks() {
        notes.u(t.notes.len() as u64);
        channels.u(t.notes.len() as u64);
        subs.u(t.notes.len() as u64);
        groups.u(t.notes.len() as u64);
        for n in t.notes.iter() {
            notes.u(n.start_tick);
            notes.u(n.duration_ticks);
            notes.bytes(&[
                n.attrs.pitch(),
                n.attrs.velocity(),
                n.attrs.off_velocity(),
                n.attrs.off_kind() as u8,
                u8::from(n.attrs.muted()),
            ]);
            channels.bytes(&[n.attrs.channel()]);
            subs.u(u64::from(n.sub.0));
            groups.u(u64::from(n.group));
        }
        for sc in t.subchannels().entries() {
            subs.u(u64::from(sc.id.0));
            subs.bytes(&[sc.midi_channel]);
            subs.text(sc.name.as_deref().unwrap_or("\u{0}"));
        }
        events.u(t.events.len() as u64);
        for e in t.events.events() {
            events.u(e.tick);
            events.u(u64::from(e.anchor));
            let mut raw = Vec::new();
            t.events.encode_event(&e.kind, &mut raw);
            events.u(raw.len() as u64);
            events.bytes(&raw);
        }
        events.u(t.end_of_track_tick.map_or(u64::MAX, |x| x));
        structure.u(u64::from(t.id().0));
        structure.u(t.parent().map_or(0, |p| u64::from(p.0) + 1));
        structure.text(t.name().unwrap_or("\u{0}"));
        let p = t.props();
        structure.bytes(&[
            p.default_channel,
            u8::from(p.kind == TrackKind::Rhythm),
            u8::from(t.mute),
            u8::from(t.solo),
        ]);
        structure.text(&format!("{:?}", p.route));
        structure.u(p.tick_correction as u64);
        structure.u(u64::from(p.correction_from_measure));
        structure.u(i64::from(p.key_correction) as u64);
        structure.u(u64::from(p.gate.hundredths()));
        structure.u(u64::from(p.key_range.0));
        structure.u(u64::from(p.key_range.1));
        structure.text(&format!("{:?}", p.onion_color));
    }
    let proj = d.project();
    for g in proj.groups.entries() {
        groups.u(u64::from(g.id));
        groups.u(u64::from(g.parent));
        groups.u(u64::from(g.link));
        groups.bytes(&[u8::from(g.locked), u8::from(g.temporary)]);
        groups.text(g.name.as_deref().unwrap_or("\u{0}"));
        groups.text(&format!("{:?}", g.color));
    }
    groups.u(u64::from(proj.groups.next_id()));
    for f in proj.folders.folders() {
        structure.u(u64::from(f.id));
        structure.text(&f.name);
        structure.u(f.anchor.map_or(0, |a| u64::from(a.0) + 1));
    }
    for (t, f) in proj.folders.members() {
        structure.u(u64::from(t.0));
        structure.u(u64::from(*f));
    }
    for c in &proj.folders_collapsed {
        structure.u(u64::from(*c));
    }
    let c = d.conductor();
    let mut tempo = SemHash::new();
    tempo.bytes(&[c.scope().code()]);
    for p in c.tempo_points() {
        tempo.u(p.tick);
        tempo.text(&format!("{:?}", p.value));
        tempo.text(&format!("{:?}", p.interp));
        tempo.bytes(&[u8::from(p.implicit)]);
    }
    for s in c.time_signatures() {
        tempo.u(s.tick);
        tempo.bytes(&[s.numerator, s.denom_pow, s.clocks, s.thirty_seconds]);
    }
    for m in c.markers() {
        tempo.u(m.start);
        tempo.u(m.end.map_or(u64::MAX, |e| e));
        tempo.bytes(&[u8::from(m.cue)]);
        tempo.bytes(m.title_bytes());
    }
    for v in c.master_volume() {
        tempo.u(v.tick);
        tempo.u(u64::from(v.value));
        tempo.bytes(&[v.device]);
    }
    vec![
        ("notes", notes.0),
        ("channels", channels.0),
        ("subchannels", subs.0),
        ("groups", groups.0),
        ("events", events.0),
        ("tempo", tempo.0),
        ("structure", structure.0),
        ("fingerprint", d.fingerprint()),
        ("state_fingerprint", d.state_fingerprint()),
    ]
}

/// The hashes as `name=hex` lines (the `.hashes` files).
fn hashes_text(d: &PlumDocument) -> String {
    semantic_hashes(d)
        .iter()
        .map(|(k, v)| format!("{k}={v:016x}\n"))
        .collect()
}
