//! v0.8 §069 (V08-0025): the built-in plugins' settings survive a restart.
//! They are application settings (`settings.json`, object `"plugins"`), never
//! project data: HZ-Chop / SAFOR keep their last values and scope (as preset
//! text), Image to MIDI and Spiderweb their window settings and paste
//! options. Every value is validated when read back; anything unreadable
//! falls back to the default without a message (settings are a convenience).

use super::PlumSpiritApp;
use super::paste_ui::PasteOptions;
use plumspirit_engine::transform::{Family, ParamSet, preset};
use serde_json::{Map, Value, json};

/// Largest `"plugins"` object kept (serialized bytes).
pub const MAX_BYTES: usize = 256 * 1024;

pub(super) fn num(o: &Map<String, Value>, k: &str, lo: f64, hi: f64) -> Option<f64> {
    o.get(k)?
        .as_f64()
        .filter(|f| f.is_finite() && (lo..=hi).contains(f))
}

pub(super) fn int(o: &Map<String, Value>, k: &str, lo: i64, hi: i64) -> Option<i64> {
    o.get(k)?.as_i64().filter(|i| (lo..=hi).contains(i))
}

pub(super) fn flag(o: &Map<String, Value>, k: &str) -> Option<bool> {
    o.get(k)?.as_bool()
}

pub(super) fn paste_json(p: &PasteOptions) -> Value {
    json!({
        "mode": p.mode,
        "offset": p.offset,
        "below": p.below,
        "channel_rule": p.channel_rule,
        "fixed_channel": p.fixed_channel,
    })
}

pub(super) fn paste_restore(p: &mut PasteOptions, v: Option<&Value>) {
    let Some(o) = v.and_then(Value::as_object) else {
        return;
    };
    use plumspirit_engine::transform::paste::{BELOW_ZERO, CHANNEL_RULES, PASTE_MODES};
    if let Some(i) = int(o, "mode", 0, PASTE_MODES.len() as i64 - 1) {
        p.mode = i as usize;
    }
    if let Some(i) = int(o, "offset", -1_000_000, 1_000_000) {
        p.offset = i;
    }
    if let Some(i) = int(o, "below", 0, BELOW_ZERO.len() as i64 - 1) {
        p.below = i as usize;
    }
    if let Some(i) = int(o, "channel_rule", 0, CHANNEL_RULES.len() as i64 - 1) {
        p.channel_rule = i as usize;
    }
    if let Some(i) = int(o, "fixed_channel", 1, 16) {
        p.fixed_channel = i as u8;
    }
}

/// A plugin tool's values and scope as JSON.
pub(super) fn tool_json(id: &str, values: &ParamSet, scope: usize) -> Value {
    let text = preset::to_text(&preset::Preset {
        tool: id.to_owned(),
        name: "last used".into(),
        values: values.clone(),
    });
    json!({ "values": text, "scope": scope })
}

/// Reads a plugin tool's saved values (only valid ones; others default).
pub(super) fn tool_restore(id: &str, v: &Value) -> Option<(ParamSet, usize)> {
    let def = plumspirit_engine::transform::tool(id)?;
    let spec = (def.spec)();
    let o = v.as_object()?;
    let text = o.get("values")?.as_str()?;
    let (p, _warnings) = preset::parse(text, id, &spec.params).ok()?;
    let scope = int(o, "scope", 0, 16).unwrap_or(0) as usize;
    Some((p.values, scope))
}

impl PlumSpiritApp {
    /// The plugins' settings now (for `settings.json`).
    pub(super) fn plugin_memory(&self) -> Map<String, Value> {
        let mut m = self.settings.plugins.clone();
        for (id, (values, scope)) in &self.tool_memory {
            let plugin = plumspirit_engine::transform::tool(id)
                .is_some_and(|d| (d.spec)().family == Family::Plugin);
            if plugin {
                m.insert(format!("tool:{id}"), tool_json(id, values, *scope));
            }
        }
        m.insert("image_to_midi".into(), self.image_midi.memory());
        m.insert("spiderweb".into(), self.spiderweb.memory());
        if serde_json::to_string(&m).map_or(true, |s| s.len() > MAX_BYTES) {
            return Map::new();
        }
        m
    }

    /// Applies the saved plugin settings (at start).
    pub(super) fn restore_plugin_memory(&mut self) {
        let m = self.settings.plugins.clone();
        for (k, v) in &m {
            if let Some(id) = k.strip_prefix("tool:")
                && let Some(def) = plumspirit_engine::transform::tool(id)
                && (def.spec)().family == Family::Plugin
                && let Some(mem) = tool_restore(def.id, v)
            {
                self.tool_memory.insert(def.id, mem);
            }
        }
        if let Some(v) = m.get("image_to_midi") {
            self.image_midi.restore(v);
        }
        if let Some(v) = m.get("spiderweb") {
            self.spiderweb.restore(v);
        }
    }

    /// Saves the settings when a plugin window closes (its settings are then
    /// final), so they survive even an unexpected end of the program.
    pub(super) fn watch_plugin_windows(&mut self) {
        let plugin_tool = self
            .tool
            .as_ref()
            .is_some_and(|s| s.spec.family == Family::Plugin);
        let now = (self.image_midi.open, self.spiderweb.open, plugin_tool);
        let was = std::mem::replace(&mut self.plugin_windows_open, now);
        if (was.0 && !now.0) || (was.1 && !now.1) || (was.2 && !now.2) {
            self.save_settings();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_tool_settings_round_trip_and_bad_values_fall_back() {
        let def = plumspirit_engine::transform::tool("hz_chop").unwrap();
        let spec = (def.spec)();
        let mut values = ParamSet::defaults(&spec.params);
        let changed = spec
            .params
            .iter()
            .find(|p| {
                matches!(
                    p.default,
                    plumspirit_engine::transform::ParamValue::Float(_)
                )
            })
            .map(|p| p.key);
        if let Some(k) = changed {
            values.set(k, plumspirit_engine::transform::ParamValue::Float(2.5));
        }
        let v = tool_json(def.id, &values, 3);
        let (back, scope) = tool_restore(def.id, &v).unwrap();
        assert_eq!(back.fingerprint(), values.fingerprint());
        assert_eq!(scope, 3);
        // Damaged entries are ignored, never a panic.
        assert!(tool_restore(def.id, &json!({"values": 5})).is_none());
        assert!(tool_restore(def.id, &json!("x")).is_none());
        assert!(tool_restore("no_such_tool", &v).is_none());
        let (_, scope) =
            tool_restore(def.id, &json!({"values": v["values"], "scope": 99})).unwrap();
        assert_eq!(scope, 0, "an impossible scope falls back to the default");
    }

    #[test]
    fn paste_options_round_trip_and_out_of_range_values_are_ignored() {
        let mut p = PasteOptions {
            mode: 0,
            offset: -7,
            below: 1,
            channel_rule: 1,
            fixed_channel: 9,
            ..PasteOptions::default()
        };
        let v = paste_json(&p);
        let mut q = PasteOptions::default();
        paste_restore(&mut q, Some(&v));
        assert_eq!(
            (q.mode, q.offset, q.below, q.channel_rule, q.fixed_channel),
            (0, -7, 1, 1, 9)
        );
        paste_restore(
            &mut p,
            Some(&json!({"mode": 99, "offset": 1e12, "fixed_channel": 0})),
        );
        assert_eq!((p.mode, p.offset, p.fixed_channel), (0, -7, 9));
    }

    /// The windows' settings survive settings.json (write → parse → apply)
    /// exactly, and a damaged "plugins" object never breaks the settings.
    #[test]
    fn window_settings_survive_a_restart_through_settings_json() {
        let mut im = crate::app::image_midi::ImageMidi::default();
        im.restore(&json!({
            "first_key": 21, "last_key": 108, "ticks_per_pixel": 12, "velocity": 90,
            "alpha_min": 40, "max_len": 64, "pixel_notes": true, "rotate": -90.0,
            "flip_x": true, "palette": ["ff0000", "00ff00"], "ignored": [false, true],
            "palette_mode": 2, "palette_size": 32, "seed": 7, "keep_aspect": false,
            "time_pixels": 300, "random_colors": true, "split": 0, "orientation": 1,
            "scale_x": -150.0, "scale_y": 50.0,
            "paste": {"mode": 0, "offset": 4, "fixed_channel": 3}
        }));
        let mut sw = crate::app::spiderweb::Spiderweb::default();
        sw.restore(&json!({
            "snap": 3, "free_smooth": 40, "vel_start": 80, "vel_end": 20,
            "channel_mode": "single", "split": "time", "where_drawn": true,
            "paste": {"mode": 1, "offset": -2}
        }));
        let mut settings = crate::settings::Settings::default();
        settings.plugins.insert("image_to_midi".into(), im.memory());
        settings.plugins.insert("spiderweb".into(), sw.memory());
        let text = crate::settings::to_json(&settings);
        let loaded = crate::settings::parse(&text);
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let mut im2 = crate::app::image_midi::ImageMidi::default();
        im2.restore(&loaded.settings.plugins["image_to_midi"]);
        assert_eq!(im2.memory(), im.memory());
        assert_eq!(im.memory()["first_key"], 21);
        assert_eq!(im.memory()["palette"], json!(["ff0000", "00ff00"]));
        let mut sw2 = crate::app::spiderweb::Spiderweb::default();
        sw2.restore(&loaded.settings.plugins["spiderweb"]);
        assert_eq!(sw2.memory(), sw.memory());
        assert_eq!(sw.memory()["channel_mode"], "single");
        // Out-of-range values keep the defaults; garbage is ignored.
        let mut im3 = crate::app::image_midi::ImageMidi::default();
        im3.restore(&json!({"first_key": 100, "last_key": 10, "velocity": 0, "palette": ["zz"]}));
        assert_eq!(
            im3.memory(),
            crate::app::image_midi::ImageMidi::default().memory()
        );
        let bad = crate::settings::parse(r#"{"version": 1, "plugins": 5}"#);
        assert!(bad.settings.plugins.is_empty());
    }
}
