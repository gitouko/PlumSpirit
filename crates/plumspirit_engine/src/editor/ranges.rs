//! Note Properties → Ranges (v1.0-beta1 B1-0078, contract §010): any number
//! of range rows (bounded by [`MAX_ROWS`]), each with its own switch, target
//! (ticks, gates, transpose, keys, velocities, channels, subchannels or a
//! controller value), the part of the selection it covers (`span`, along
//! time, key, note order or velocity), the values it runs through (`from` →
//! `to`, through its own graph — per-point smoothness and Overshoot, §017)
//! and how it combines with what is already there (`blend`).
//!
//! Rows apply **in order, top to bottom**: a later row works on the value the
//! rows above it left, so overlapping rows are deterministic — with Set the
//! last one wins, Add / Multiply accumulate, Min / Max cap.
//! [`overlaps`] lists where two rows of the same target meet, for the
//! window's explanation.
//!
//! The rows travel as one text parameter (presets, Undo, Preview are the
//! platform's): rows separated by `|`, fields by `~`:
//! `on~target[:cc]~across~lo~hi~from~to~blend~graph`, the graph in
//! [`Curve::to_text`] form (it never contains `|` or `~`).

use crate::transform::param::Curve;

/// Most rows (memory-bounded; far more than a window shows at once).
pub const MAX_ROWS: usize = 1024;
/// Longest rows text (presets are at most 256 KB).
pub const MAX_TEXT: usize = 200_000;
/// The largest |value| a row may hold (ticks; beyond any real project).
pub const MAX_VALUE: i64 = 1 << 50;

/// What a row changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// Start offset in ticks (Set: from the note's own start).
    Ticks,
    /// Length in ticks (at least 1).
    Gates,
    /// Key offset in semitones (Set: from the note's own key).
    Transpose,
    /// Absolute key.
    Keys,
    /// Velocity 1 – 127.
    Velocities,
    /// MIDI channel 1 – 16.
    Channels,
    /// Subchannel: its place in the track's list (0 = the first).
    Subchannels,
    /// A controller value 0 – 127 at the note's start, on its channel
    /// (controllers 0 – [`MAX_CONTROLLER`]).
    Controller(u8),
}

/// The highest controller a range (or a Note Properties formula) writes:
/// CC 120 – 127 are channel-mode messages (All Notes Off, Reset All
/// Controllers …) that the controller editor only adds one at a time.
pub const MAX_CONTROLLER: u8 = 119;

impl Target {
    /// The targets offered (the controller with its default number).
    pub const ALL: [Target; 8] = [
        Target::Velocities,
        Target::Gates,
        Target::Ticks,
        Target::Transpose,
        Target::Keys,
        Target::Channels,
        Target::Subchannels,
        Target::Controller(1),
    ];

    pub fn name(self) -> &'static str {
        match self {
            Target::Ticks => "Ticks",
            Target::Gates => "Gates",
            Target::Transpose => "Transpose",
            Target::Keys => "Keys",
            Target::Velocities => "Velocities",
            Target::Channels => "Channels",
            Target::Subchannels => "Subchannels",
            Target::Controller(_) => "Controller",
        }
    }

    fn key(self) -> String {
        match self {
            Target::Ticks => "ticks".into(),
            Target::Gates => "gates".into(),
            Target::Transpose => "transpose".into(),
            Target::Keys => "keys".into(),
            Target::Velocities => "vel".into(),
            Target::Channels => "ch".into(),
            Target::Subchannels => "sub".into(),
            Target::Controller(n) => format!("cc:{n}"),
        }
    }

    fn from_key(k: &str) -> Option<Target> {
        Some(match k {
            "ticks" => Target::Ticks,
            "gates" => Target::Gates,
            "transpose" => Target::Transpose,
            "keys" => Target::Keys,
            "vel" => Target::Velocities,
            "ch" => Target::Channels,
            "sub" => Target::Subchannels,
            _ => {
                let n: u8 = k.strip_prefix("cc:")?.parse().ok()?;
                if n > MAX_CONTROLLER {
                    return None;
                }
                Target::Controller(n)
            }
        })
    }

    /// Whether two targets change the same thing (offsets and absolute
    /// values of the same property count as the same).
    pub fn same_property(self, o: Target) -> bool {
        let p = |t: Target| match t {
            Target::Transpose | Target::Keys => 0u16,
            Target::Ticks => 1,
            Target::Gates => 2,
            Target::Velocities => 3,
            Target::Channels => 4,
            Target::Subchannels => 5,
            Target::Controller(n) => 100 + u16::from(n),
        };
        p(self) == p(o)
    }

    /// The default values a new row runs through.
    pub fn defaults(self) -> (i64, i64) {
        match self {
            Target::Ticks => (0, 120),
            Target::Gates => (60, 480),
            Target::Transpose => (0, 12),
            Target::Keys => (48, 72),
            Target::Velocities => (40, 120),
            Target::Channels => (1, 16),
            Target::Subchannels => (0, 3),
            Target::Controller(_) => (0, 127),
        }
    }

    /// The unit shown next to the values.
    pub fn unit(self) -> &'static str {
        match self {
            Target::Ticks | Target::Gates => "ticks",
            Target::Transpose => "semitones",
            Target::Keys => "key",
            Target::Velocities => "velocity",
            Target::Channels => "channel",
            Target::Subchannels => "place",
            Target::Controller(_) => "value",
        }
    }
}

/// What a row's span and graph run along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Across {
    /// The selection's time, first start → last start.
    Time,
    /// Its keys, lowest → highest.
    Key,
    /// The notes in time order, first → last.
    Order,
    /// Its velocities, softest → loudest.
    Velocity,
}

impl Across {
    pub const ALL: [Across; 4] = [Across::Time, Across::Key, Across::Order, Across::Velocity];

    pub fn name(self) -> &'static str {
        match self {
            Across::Time => "Time",
            Across::Key => "Key",
            Across::Order => "Note order",
            Across::Velocity => "Velocity",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Across::Time => "time",
            Across::Key => "key",
            Across::Order => "order",
            Across::Velocity => "vel",
        }
    }

    fn from_key(k: &str) -> Option<Across> {
        Across::ALL.into_iter().find(|a| a.key() == k)
    }
}

/// How a row's value combines with the value the rows above left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    /// The row's value (for Ticks / Transpose: offset from the note's own).
    Set,
    /// Added.
    Add,
    /// Multiplied by value / 100 (a percentage).
    Multiply,
    /// At most the row's value.
    Min,
    /// At least the row's value.
    Max,
}

impl Blend {
    pub const ALL: [Blend; 5] = [
        Blend::Set,
        Blend::Add,
        Blend::Multiply,
        Blend::Min,
        Blend::Max,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Blend::Set => "Set",
            Blend::Add => "Add",
            Blend::Multiply => "Multiply %",
            Blend::Min => "At most",
            Blend::Max => "At least",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Blend::Set => "set",
            Blend::Add => "add",
            Blend::Multiply => "mul",
            Blend::Min => "min",
            Blend::Max => "max",
        }
    }

    fn from_key(k: &str) -> Option<Blend> {
        Blend::ALL.into_iter().find(|b| b.key() == k)
    }

    /// `cur` combined with the row's value `v`; `base` is the note's own
    /// value (Set on an offset target adds `v` to it).
    pub fn apply(self, cur: f64, v: f64, base: f64, offset: bool) -> f64 {
        match self {
            Blend::Set if offset => base + v,
            Blend::Set => v,
            Blend::Add => cur + v,
            Blend::Multiply => cur * v / 100.0,
            Blend::Min => cur.min(v),
            Blend::Max => cur.max(v),
        }
    }
}

/// One range row.
#[derive(Clone, Debug, PartialEq)]
pub struct RangeRow {
    pub on: bool,
    pub target: Target,
    pub across: Across,
    /// The part of the selection the row covers, 0 – 1 along `across`
    /// (inclusive; `lo ≤ hi`).
    pub span: (f64, f64),
    /// The values at the span's start and end (through the graph).
    pub from: i64,
    pub to: i64,
    pub blend: Blend,
    /// The graph from `from` (0) to `to` (1) across the span; it may
    /// overshoot (§017).
    pub graph: Curve,
}

impl RangeRow {
    /// A new row for `target` over the whole selection.
    pub fn new(target: Target) -> RangeRow {
        let (from, to) = target.defaults();
        RangeRow {
            on: true,
            target,
            across: Across::Time,
            span: (0.0, 1.0),
            from,
            to,
            blend: Blend::Set,
            graph: Curve::identity(),
        }
    }

    /// The row's value at position `u` (0 – 1 along `across`), or `None`
    /// outside its span or when it is off.
    #[inline]
    pub fn value_at(&self, u: f64) -> Option<f64> {
        let (lo, hi) = self.span;
        if !self.on || !(lo..=hi).contains(&u) {
            return None;
        }
        let x = if hi > lo { (u - lo) / (hi - lo) } else { 0.0 };
        Some(self.from as f64 + (self.to - self.from) as f64 * self.graph.eval(x))
    }

    fn to_text(&self) -> String {
        format!(
            "{}~{}~{}~{}~{}~{}~{}~{}~{}",
            u8::from(self.on),
            self.target.key(),
            self.across.key(),
            self.span.0,
            self.span.1,
            self.from,
            self.to,
            self.blend.key(),
            self.graph.to_text()
        )
    }

    fn from_text(t: &str) -> Result<RangeRow, String> {
        let f: Vec<&str> = t.split('~').map(str::trim).collect();
        if f.len() != 9 {
            return Err(format!("a range needs 9 fields, this one has {}", f.len()));
        }
        let on = match f[0] {
            "1" => true,
            "0" => false,
            x => return Err(format!("\"{x}\" is not on (1) or off (0)")),
        };
        if let Some(n) = f[1].strip_prefix("cc:").and_then(|n| n.parse::<u8>().ok())
            && (MAX_CONTROLLER + 1..=127).contains(&n)
        {
            return Err(format!(
                "CC {n} is a channel-mode message (CC 120 – 127: All Notes Off and the like); a range writes CC 0 – {MAX_CONTROLLER}"
            ));
        }
        let target =
            Target::from_key(f[1]).ok_or_else(|| format!("unknown target \"{}\"", f[1]))?;
        let across = Across::from_key(f[2]).ok_or_else(|| format!("unknown axis \"{}\"", f[2]))?;
        let num = |s: &str| -> Result<f64, String> {
            s.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .ok_or_else(|| format!("\"{s}\" is not a share from 0 to 1"))
        };
        let (lo, hi) = (num(f[3])?, num(f[4])?);
        let int = |s: &str| -> Result<i64, String> {
            s.parse::<i64>()
                .ok()
                .filter(|v| v.abs() <= MAX_VALUE)
                .ok_or_else(|| format!("\"{s}\" is not a whole number within ±2^50"))
        };
        let (from, to) = (int(f[5])?, int(f[6])?);
        let blend = Blend::from_key(f[7]).ok_or_else(|| format!("unknown blend \"{}\"", f[7]))?;
        let graph = Curve::from_text(f[8]).ok_or_else(|| "the graph is not valid".to_owned())?;
        Ok(RangeRow {
            on,
            target,
            across,
            span: (lo.min(hi), lo.max(hi)),
            from,
            to,
            blend,
            graph,
        })
    }
}

/// The rows as their parameter text.
pub fn to_text(rows: &[RangeRow]) -> String {
    rows.iter()
        .map(RangeRow::to_text)
        .collect::<Vec<_>>()
        .join("|")
}

/// Parses the rows parameter (empty text = no rows). Errors name the row.
pub fn parse(text: &str) -> Result<Vec<RangeRow>, String> {
    if text.len() > MAX_TEXT {
        return Err(format!(
            "The ranges are longer than {} KB.",
            MAX_TEXT / 1000
        ));
    }
    let t = text.trim();
    if t.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<&str> = t.split('|').collect();
    if parts.len() > MAX_ROWS {
        return Err(format!("At most {MAX_ROWS} ranges, not {}.", parts.len()));
    }
    parts
        .iter()
        .enumerate()
        .map(|(i, p)| RangeRow::from_text(p).map_err(|e| format!("Range {}: {e}.", i + 1)))
        .collect()
}

/// Two switched-on rows of the same property whose spans meet on the same
/// axis: where, and which applies last.
#[derive(Clone, Debug, PartialEq)]
pub struct Overlap {
    /// The earlier and the later row (0-based; the later one applies last).
    pub first: usize,
    pub last: usize,
    /// The shared part of the span (0 – 1).
    pub span: (f64, f64),
}

/// Every overlap between switched-on rows of the same property along the
/// same axis (rows on different axes may also meet note by note; they are
/// listed with the full span when both cover all of it). O(rows²) — rows
/// are few.
pub fn overlaps(rows: &[RangeRow]) -> Vec<Overlap> {
    let mut out = Vec::new();
    for (a, ra) in rows.iter().enumerate() {
        for (b, rb) in rows.iter().enumerate().skip(a + 1) {
            if !(ra.on && rb.on && ra.target.same_property(rb.target)) {
                continue;
            }
            let span = if ra.across == rb.across {
                (ra.span.0.max(rb.span.0), ra.span.1.min(rb.span.1))
            } else if ra.span == (0.0, 1.0) && rb.span == (0.0, 1.0) {
                (0.0, 1.0)
            } else {
                continue;
            };
            if span.0 <= span.1 {
                out.push(Overlap {
                    first: a,
                    last: b,
                    span,
                });
            }
        }
    }
    out
}

/// A sentence explaining an overlap (the window's tooltip / list).
pub fn explain(rows: &[RangeRow], o: &Overlap) -> String {
    let b = &rows[o.last];
    let how = match b.blend {
        Blend::Set => "replaces",
        Blend::Add => "adds to",
        Blend::Multiply => "scales",
        Blend::Min => "caps",
        Blend::Max => "raises",
    };
    format!(
        "Ranges {} and {} meet ({} {:.0} % – {:.0} %): range {} applies last and {how} what range {} set.",
        o.first + 1,
        o.last + 1,
        b.across.name().to_lowercase(),
        o.span.0 * 100.0,
        o.span.1 * 100.0,
        o.last + 1,
        o.first + 1
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::param::Outside;

    #[test]
    fn rows_round_trip_as_text_and_refuse_invalid_fields() {
        let mut a = RangeRow::new(Target::Velocities);
        a.span = (0.25, 0.75);
        a.graph = Curve::new(vec![(0.0, 0.0), (0.5, 1.3), (1.0, 0.2)])
            .with_smoothness(0.5)
            .with_overshoot(0.5)
            .with_outside(Outside::Extend)
            .with_local(1, Some(0.0))
            .sanitized();
        let mut b = RangeRow::new(Target::Controller(74));
        b.on = false;
        b.across = Across::Key;
        b.blend = Blend::Max;
        let rows = vec![a, b, RangeRow::new(Target::Ticks)];
        let t = to_text(&rows);
        assert_eq!(parse(&t).unwrap(), rows, "{t}");
        assert!(t.contains("cc:74"), "{t}");
        assert_eq!(parse("").unwrap(), Vec::<RangeRow>::new());
        assert_eq!(parse("   ").unwrap(), Vec::<RangeRow>::new());
        for bad in [
            "1~vel~time~0~1~40~120~set",
            "2~vel~time~0~1~40~120~set~0;0,0;1,1",
            "1~volume~time~0~1~40~120~set~0;0,0;1,1",
            "1~vel~space~0~1~40~120~set~0;0,0;1,1",
            "1~vel~time~0~1.5~40~120~set~0;0,0;1,1",
            "1~vel~time~0~1~4.5~120~set~0;0,0;1,1",
            "1~vel~time~0~1~40~120~mix~0;0,0;1,1",
            "1~vel~time~0~1~40~120~set~9;0,0;1,1",
            "1~cc:200~time~0~1~40~120~set~0;0,0;1,1",
            "1~cc:120~time~0~1~40~120~set~0;0,0;1,1",
            "1~cc:127~time~0~1~40~120~set~0;0,0;1,1",
            "1~vel~time~0~1~9999999999999999~120~set~0;0,0;1,1",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let e = parse("1~vel~time~0~1~40~120~set~0;0,0;1,1|1~x~time~0~1~0~1~set~0;0,0;1,1")
            .unwrap_err();
        assert!(e.starts_with("Range 2: unknown target"), "{e}");
        let e = parse("1~cc:123~time~0~1~40~120~set~0;0,0;1,1").unwrap_err();
        assert!(e.contains("CC 123 is a channel-mode message"), "{e}");
        assert!(parse("1~cc:119~time~0~1~40~120~set~0;0,0;1,1").is_ok());
        // Reversed spans are stored in order.
        let r = parse("1~vel~time~0.8~0.2~40~120~set~0;0,0;1,1").unwrap();
        assert_eq!(r[0].span, (0.2, 0.8));
        let many = vec![RangeRow::new(Target::Gates); MAX_ROWS + 1];
        assert!(parse(&to_text(&many)).is_err());
        assert_eq!(parse(&to_text(&many[..MAX_ROWS])).unwrap().len(), MAX_ROWS);
    }

    #[test]
    fn a_row_runs_through_its_graph_inside_its_span() {
        let mut r = RangeRow::new(Target::Velocities);
        r.span = (0.5, 1.0);
        assert_eq!(r.value_at(0.25), None);
        assert_eq!(r.value_at(0.5), Some(40.0));
        assert_eq!(r.value_at(0.75), Some(80.0));
        assert_eq!(r.value_at(1.0), Some(120.0));
        r.graph = Curve::new(vec![(0.0, 0.0), (1.0, 1.5)]).with_overshoot(0.5);
        assert_eq!(
            r.value_at(1.0),
            Some(160.0),
            "overshoot passes the To value"
        );
        r.on = false;
        assert_eq!(r.value_at(0.75), None);
        // A zero-width span covers its point.
        let mut p = RangeRow::new(Target::Gates);
        p.span = (0.3, 0.3);
        assert_eq!(p.value_at(0.3), Some(60.0));
        assert_eq!(p.value_at(0.31), None);
    }

    #[test]
    fn blends_combine_in_order_and_overlaps_are_listed() {
        assert_eq!(Blend::Set.apply(50.0, 80.0, 50.0, false), 80.0);
        assert_eq!(
            Blend::Set.apply(70.0, 5.0, 60.0, true),
            65.0,
            "an offset from the note's own"
        );
        assert_eq!(Blend::Add.apply(70.0, 5.0, 60.0, false), 75.0);
        assert_eq!(Blend::Multiply.apply(80.0, 50.0, 0.0, false), 40.0);
        assert_eq!(Blend::Min.apply(80.0, 60.0, 0.0, false), 60.0);
        assert_eq!(Blend::Max.apply(80.0, 90.0, 0.0, false), 90.0);
        let mut a = RangeRow::new(Target::Velocities);
        a.span = (0.0, 0.6);
        let mut b = RangeRow::new(Target::Velocities);
        b.span = (0.4, 1.0);
        b.blend = Blend::Add;
        let c = RangeRow::new(Target::Gates);
        let mut d = RangeRow::new(Target::Transpose);
        d.across = Across::Key;
        let e = RangeRow::new(Target::Keys);
        let rows = vec![a, b, c, d, e];
        let o = overlaps(&rows);
        assert_eq!(
            o,
            vec![
                Overlap {
                    first: 0,
                    last: 1,
                    span: (0.4, 0.6)
                },
                Overlap {
                    first: 3,
                    last: 4,
                    span: (0.0, 1.0)
                },
            ]
        );
        let s = explain(&rows, &o[0]);
        assert_eq!(
            s,
            "Ranges 1 and 2 meet (time 40 % – 60 %): range 2 applies last and adds to what range 1 set."
        );
        let mut off = rows.clone();
        off[1].on = false;
        assert_eq!(overlaps(&off).len(), 1);
    }
}
