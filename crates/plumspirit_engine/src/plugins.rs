//! The v0.8 built-in plugins: identity, credit and help (contract §025, §069).
//!
//! These are internal parts of PlumSpirit, called "plugins" in the interface;
//! there is no public plugin API (that is planned for a later version). HZ-Chop
//! and SAFOR run as transform-platform tools (`transform::tools::plugins`);
//! Image to MIDI, Spiderweb and Audio Reference have their own windows.
//! Every claim here is verified in `docs/references/v0_8/UPSTREAM_LOCK.md`.

/// A built-in plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginId {
    Spiderweb,
    ImageToMidi,
    HzChop,
    AudioReference,
    Safor,
}

/// Where a plugin works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginKind {
    /// Works on the Piano Roll's notes (owner's term "Piano Roll Plugin").
    PianoRoll,
    /// Works on the whole project ("General Plugin").
    General,
}

impl PluginKind {
    pub fn label(self) -> &'static str {
        match self {
            PluginKind::PianoRoll => "Piano Roll Plugin",
            PluginKind::General => "General Plugin",
        }
    }
}

/// Identity, credit and help of a built-in plugin.
#[derive(Clone, Copy, Debug)]
pub struct PluginInfo {
    pub id: PluginId,
    /// The original name, kept recognisable.
    pub name: &'static str,
    /// Longer display name where the original has one.
    pub full_name: &'static str,
    pub kind: PluginKind,
    /// The original author(s).
    pub author: &'static str,
    /// The original project.
    pub upstream: &'static str,
    /// The original's licence as verified ("none stated" when it has none).
    pub licence: &'static str,
    /// How PlumSpirit's version relates to the original.
    pub relation: &'static str,
    /// The transform tool id when the plugin runs as a tool.
    pub tool: Option<&'static str>,
    /// What it does (Help).
    pub help: &'static str,
}

impl PluginInfo {
    /// One credit line ("HZ-Chop by Softy107 — native rewrite for PlumSpirit").
    pub fn credit(&self) -> String {
        format!("{} by {} — {}", self.name, self.author, self.relation)
    }
}

pub static PLUGINS: &[PluginInfo] = &[
    PluginInfo {
        id: PluginId::Spiderweb,
        name: "Spiderweb",
        full_name: "Spiderweb",
        kind: PluginKind::PianoRoll,
        author: "Kanade Tachibana",
        upstream: "https://github.com/UnPrioritized/Spiderweb",
        licence: "MIT",
        relation: "expanded for PlumSpirit by ouko",
        tool: None,
        help: "Draws note art from shapes, curves and text and sends it straight to the Piano Roll, with colors going to new Subtracks or to Subchannels of one Track.",
    },
    PluginInfo {
        id: PluginId::ImageToMidi,
        name: "Image to MIDI",
        full_name: "Image to MIDI",
        kind: PluginKind::PianoRoll,
        author: "arduano (ImageToMidi)",
        upstream: "https://github.com/arduano/ImageToMidi",
        licence: "none stated (no code reused; behavior reimplemented)",
        relation: "native PlumSpirit implementation, expanded by ouko",
        tool: None,
        help: "Turns a picture into notes: one key per pixel row, neighboring pixels of one color joined into one note, each palette color going to its own Subtrack or Subchannel.",
    },
    PluginInfo {
        id: PluginId::HzChop,
        name: "HZ-Chop",
        full_name: "HZ-Chop",
        kind: PluginKind::PianoRoll,
        author: "Softy107",
        upstream: "HZ-Chop for Andromeda (Lua plugin)",
        licence: "none stated (behavior rewritten in Rust)",
        relation: "native rewrite for PlumSpirit",
        tool: Some("hz_chop"),
        help: "Chops the selected notes into trains of short notes whose length follows the frequency of the key at the song's tempo.",
    },
    PluginInfo {
        id: PluginId::AudioReference,
        name: "Audio Reference",
        full_name: "Audio Reference",
        kind: PluginKind::PianoRoll,
        author: "Khang (khang06, dominohook's Reference Audio)",
        upstream: "https://github.com/khang06/dominohook",
        licence: "none stated (original implementation; no code reused)",
        relation: "original implementation for PlumSpirit",
        tool: None,
        help: "Plays an audio file in sync with the song (offset, volume, loop and speed follow the transport) so you can chart notes against a recording.",
    },
    PluginInfo {
        id: PluginId::Safor,
        name: "SAFOR",
        full_name: "SAFOR — Simple And Fast Overlaps Remover",
        kind: PluginKind::General,
        author: "DixelU",
        upstream: "https://github.com/DixelU/SAFOR",
        licence: "Apache-2.0",
        relation: "adapted for PlumSpirit (structure-preserving rewrite)",
        tool: Some("safor"),
        help: "Removes doubled notes that start on the same tick and key, optionally cutting sustains under notes laid over them, while the picture stays the same.",
    },
];

pub fn info(id: PluginId) -> &'static PluginInfo {
    PLUGINS
        .iter()
        .find(|p| p.id == id)
        .expect("every plugin has an entry")
}

/// The plugin a transform tool belongs to.
pub fn for_tool(tool_id: &str) -> Option<&'static PluginInfo> {
    PLUGINS.iter().find(|p| p.tool == Some(tool_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_plugin_is_credited_once_and_tool_plugins_exist() {
        for id in [
            PluginId::Spiderweb,
            PluginId::ImageToMidi,
            PluginId::HzChop,
            PluginId::AudioReference,
            PluginId::Safor,
        ] {
            assert_eq!(PLUGINS.iter().filter(|p| p.id == id).count(), 1);
            let i = info(id);
            assert!(!i.author.is_empty() && !i.licence.is_empty() && !i.help.is_empty());
            if let Some(t) = i.tool {
                assert!(crate::transform::tool(t).is_some(), "{t}");
            }
        }
        assert!(
            info(PluginId::Spiderweb)
                .credit()
                .contains("expanded for PlumSpirit by ouko")
        );
        assert_eq!(for_tool("safor").map(|p| p.id), Some(PluginId::Safor));
    }
}
