//! Tool presets: a tool configuration (never musical content — that is a
//! `.psc` score). Plain UTF-8 text, one `key=value` per line, deterministic
//! (keys sorted), validated against the tool's parameter specs. Presets are
//! data only: nothing in them is ever executed (contract §146).
//!
//! ```text
//! PlumSpirit Preset 1
//! tool=humanize
//! name=Loose drummer
//! p.tick.max=12
//! p.tick.min=-12
//! ```

use super::param::{ParamSet, ParamSpec, ParamValue};

pub const MAGIC: &str = "PlumSpirit Preset";
pub const VERSION: u32 = 1;
/// Largest preset file accepted.
pub const MAX_BYTES: usize = 256 * 1024;
/// Most lines accepted.
pub const MAX_LINES: usize = 4096;
/// Longest preset name.
pub const MAX_NAME_CHARS: usize = 100;
/// Longest execution order accepted (B2-F01; far more than 99 ids).
pub const ORDER_MAX_CHARS: usize = 4096;

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub tool: String,
    pub name: String,
    pub values: ParamSet,
}

/// Serializes deterministically (same preset → same bytes).
pub fn to_text(p: &Preset) -> String {
    let mut s = format!(
        "{MAGIC} {VERSION}\ntool={}\nname={}\n",
        p.tool,
        clean_name(&p.name)
    );
    for (k, v) in p.values.iter() {
        s.push_str("p.");
        s.push_str(k);
        s.push('=');
        s.push_str(&v.to_text());
        s.push('\n');
    }
    s
}

/// A display-safe name: no control characters, trimmed, ≤ 100 characters.
pub fn clean_name(n: &str) -> String {
    n.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Parses and validates a preset for a tool with `specs`. Unknown keys (from
/// newer versions) are skipped and reported; invalid values refuse the file.
pub fn parse(text: &str, tool: &str, specs: &[ParamSpec]) -> Result<(Preset, Vec<String>), String> {
    if text.len() > MAX_BYTES {
        return Err(format!(
            "The preset is larger than {} KB, so it is not a PlumSpirit preset.",
            MAX_BYTES / 1024
        ));
    }
    let mut lines = text.lines();
    let head = lines
        .next()
        .unwrap_or("")
        .trim_start_matches('\u{feff}')
        .trim();
    let version = head
        .strip_prefix(MAGIC)
        .map(str::trim)
        .ok_or("This file is not a PlumSpirit preset.")?;
    let version: u32 = version
        .parse()
        .map_err(|_| "The preset's version line is damaged.".to_string())?;
    if version > VERSION {
        return Err(format!(
            "This preset was saved by a newer PlumSpirit (preset version {version}); update PlumSpirit to load it."
        ));
    }
    let mut tool_name = None;
    let mut name = String::new();
    let mut values = ParamSet::new();
    let mut skipped = Vec::new();
    for (i, line) in lines.enumerate() {
        if i >= MAX_LINES {
            return Err("The preset has too many lines.".into());
        }
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("Line {} of the preset is not \"key=value\".", i + 2))?;
        match k {
            "tool" => tool_name = Some(v.trim().to_string()),
            "name" => name = clean_name(v),
            _ => {
                let Some(key) = k.strip_prefix("p.") else {
                    skipped.push(k.to_string());
                    continue;
                };
                // B2-F01: the execution order (stable function ids; checked
                // when the tool runs, like every other value of the tool).
                if key == super::order::ORDER_KEY {
                    if v.len() > ORDER_MAX_CHARS {
                        return Err("The preset's execution order is too long.".into());
                    }
                    values.set(key, ParamValue::Text(v.trim().to_string()));
                    continue;
                }
                let Some(spec) = specs.iter().find(|s| s.key == key) else {
                    skipped.push(key.to_string());
                    continue;
                };
                let val = ParamValue::parse(&spec.kind, v).ok_or_else(|| {
                    format!("The preset's value for \"{}\" is not valid.", spec.label)
                })?;
                values.set(key, val);
            }
        }
    }
    let t = tool_name.ok_or("The preset does not say which tool it is for.")?;
    // v0.9 §238: a removed tool's preset is named, never applied elsewhere.
    if let Some(notice) = super::registry::removed_tool(&t) {
        return Err(notice.to_owned());
    }
    if t != tool {
        return Err(format!(
            "This preset is for the tool \"{t}\", not \"{tool}\"."
        ));
    }
    Ok((
        Preset {
            tool: t,
            name,
            values,
        },
        skipped,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::param::{Curve, Outside, choice, curve, float, int};

    fn specs() -> Vec<ParamSpec> {
        vec![
            int("n", "N", 0, 10, 2),
            float("f", "F", 0.0, 1.0, 0.5, ""),
            choice("c", "C", &["a", "b", "c"], 0),
            curve("k", "K", Curve::identity()),
        ]
    }

    #[test]
    fn presets_roundtrip_byte_identically() {
        let mut v = ParamSet::defaults(&specs());
        v.set("n", ParamValue::Int(7));
        let p = Preset {
            tool: "x".into(),
            name: "My\u{7}preset ".into(),
            values: v,
        };
        let t = to_text(&p);
        let (q, skipped) = parse(&t, "x", &specs()).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(q.name, "Mypreset");
        assert_eq!(to_text(&q), t);
    }

    #[test]
    fn precise_float_and_curve_values_survive_preset_reopen() {
        let mut values = ParamSet::defaults(&specs());
        let curve = Curve::new(vec![(0.123456789012345, 1e-9), (0.9, 0.812345678901234)]);
        values.set("f", ParamValue::Float(0.123456789012345));
        values.set("k", ParamValue::Curve(curve));
        let preset = Preset {
            tool: "x".into(),
            name: "precise".into(),
            values,
        };
        let text = to_text(&preset);
        let (reopened, skipped) = parse(&text, "x", &specs()).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(reopened.values, preset.values);
    }

    #[test]
    fn an_execution_order_round_trips_and_old_presets_have_none() {
        // B2-F01: the order travels as `p.order`; a preset without it (every
        // preset before beta2) leaves the declared order.
        let mut v = ParamSet::defaults(&specs());
        v.set(
            crate::transform::order::ORDER_KEY,
            ParamValue::Text("n06,n08,m_overlaps,m_pitch_offset".into()),
        );
        let p = Preset {
            tool: "x".into(),
            name: "ordered".into(),
            values: v,
        };
        let t = to_text(&p);
        assert!(
            t.contains("\np.order=n06,n08,m_overlaps,m_pitch_offset\n"),
            "{t}"
        );
        let (q, skipped) = parse(&t, "x", &specs()).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(q.values, p.values);
        assert_eq!(to_text(&q), t);
        let (old, _) = parse("PlumSpirit Preset 1\ntool=x\np.n=3\n", "x", &specs()).unwrap();
        assert!(old.values.get_raw("order").is_none());
        let long = format!(
            "PlumSpirit Preset 1\ntool=x\np.order={}\n",
            "n".repeat(5000)
        );
        assert!(parse(&long, "x", &specs()).unwrap_err().contains("order"));
    }

    #[test]
    fn malformed_presets_are_refused_with_sentences() {
        let s = specs();
        assert!(parse("hello", "x", &s).is_err());
        assert!(
            parse("PlumSpirit Preset 99\ntool=x\n", "x", &s)
                .unwrap_err()
                .contains("newer")
        );
        assert!(parse("PlumSpirit Preset 1\ntool=y\n", "x", &s).is_err());
        assert!(parse("PlumSpirit Preset 1\ntool=x\np.c=9\n", "x", &s).is_err());
        assert!(parse("PlumSpirit Preset 1\ntool=x\nnoequals\n", "x", &s).is_err());
        let big = "x".repeat(MAX_BYTES + 1);
        assert!(parse(&big, "x", &s).is_err());
        // A removed tool's preset gives its notice, wherever it is opened.
        for (id, notice) in crate::transform::REMOVED_TOOLS {
            let text = format!("PlumSpirit Preset 1\ntool={id}\nname=old\n");
            assert_eq!(parse(&text, "x", &s).unwrap_err(), *notice);
            assert_eq!(parse(&text, id, &s).unwrap_err(), *notice);
        }
        let (p, skipped) = parse("PlumSpirit Preset 1\ntool=x\np.future=1\n", "x", &s).unwrap();
        assert_eq!(skipped, vec!["future".to_string()]);
        assert!(p.values.is_empty());
    }

    #[test]
    fn curve_presets_keep_free_ends_smoothness_and_policy_and_read_v08_curves() {
        // v0.9 §104 / §105 / §107: save, reload, everything where it was.
        let mut v = ParamSet::defaults(&specs());
        let c = Curve::new(vec![(0.15, 0.4), (0.5, 0.95), (0.85, 0.05)])
            .with_smoothness(0.5)
            .with_outside(Outside::Reflect);
        v.set("k", ParamValue::Curve(c.clone()));
        let p = Preset {
            tool: "x".into(),
            name: "free".into(),
            values: v,
        };
        let t = to_text(&p);
        let (q, _) = parse(&t, "x", &specs()).unwrap();
        assert_eq!(q.values.get_raw("k"), Some(&ParamValue::Curve(c)));
        assert_eq!(to_text(&q), t);
        // A v0.8 preset (smooth flag) loads unchanged and writes the same bytes.
        let v08 = format!(
            "{MAGIC} 1\ntool=x\nname=old\np.k=1;0.000000,0.000000;0.400000,0.700000;1.000000,1.000000\n"
        );
        let (o, skipped) = parse(&v08, "x", &specs()).unwrap();
        assert!(skipped.is_empty());
        let Some(ParamValue::Curve(k)) = o.values.get_raw("k") else {
            panic!("no curve")
        };
        assert_eq!((k.smoothness, k.outside), (1.0, Outside::Clamp));
        assert_eq!(k.points, vec![(0.0, 0.0), (0.4, 0.7), (1.0, 1.0)]);
        assert!(
            to_text(&o).ends_with("p.k=1;0.000000,0.000000;0.400000,0.700000;1.000000,1.000000\n")
        );
    }
}
