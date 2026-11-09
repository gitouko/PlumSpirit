//! The example plugins that ship with PlumSpirit (B1-0049,
//! `docs/LUA_API.md` "Examples"). Their files live in
//! `crates/plumspirit/examples-lua/` and are compiled into the program, so
//! Plugins → Lua Plugins → Examples can install them on any PC without a
//! download: [`Example::write_to`] writes one into a fresh folder, which is
//! installed like any other package (and marked [`super::package::Origin::Example`]).

use std::path::{Path, PathBuf};

/// One example plugin package.
#[derive(Clone, Copy, Debug)]
pub struct Example {
    /// The manifest's id.
    pub id: &'static str,
    /// The button's text.
    pub name: &'static str,
    /// The button's tooltip.
    pub summary: &'static str,
    /// The package's files: relative path → text.
    pub files: &'static [(&'static str, &'static str)],
}

/// Every example, in the order the window lists them.
pub const EXAMPLES: [Example; 22] = [
    Example {
        id: "plumspirit.example.graph-shift",
        name: "Graph Shift",
        summary: "A custom note tool (Tools → Custom): shifts each selected note up or down by a graph drawn over the selection.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/graph-shift/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/graph-shift/main.lua"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.cc-batch",
        name: "CC Batch",
        summary: "Two commands (Plugins menu): write a controller curve or LFO across the selection in one Undo step, and clear it.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/cc-batch/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/cc-batch/main.lua"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.custom-graph-laboratory",
        name: "Custom Graph Laboratory",
        summary: "Three Lua-authored interactive graphs: Bézier, polar rose and a MIDI envelope, with presets and deterministic Preview/Apply.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/custom-graph-laboratory/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/custom-graph-laboratory/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/custom-graph-laboratory/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.custom-drawing-studio",
        name: "Custom Drawing Studio",
        summary: "Five Lua-authored Piano Roll tools: freehand, rectangle, seeded spray, note-onset eraser and mathematical wave.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/custom-drawing-studio/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/custom-drawing-studio/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/custom-drawing-studio/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.ui-layout-laboratory",
        name: "UI Layout Laboratory",
        summary: "Native tabs, a512option list, hierarchical tree, RGBA/gradient controls, presets and author-owned canvas coordinates; no project authority.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/ui-layout-laboratory/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/ui-layout-laboratory/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/ui-layout-laboratory/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.ui-image-laboratory",
        name: "UI Image Laboratory",
        summary: "Bounded native canvas images: user-selected files, authored alpha checker, UV crop, tint and independent pointer overlay; no project authority.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/ui-image-laboratory/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/ui-image-laboratory/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/ui-image-laboratory/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.transport-monitor",
        name: "Transport Monitor",
        summary: "Read-only exact transport snapshots and bounded worker observations; no note access or playback control.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/transport-monitor/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/transport-monitor/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/transport-monitor/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.transport-controls",
        name: "Transport Controls",
        summary: "Explicit native play/pause/stop, exact tick seek and audition speed with bounded conflict-checked requests.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/transport-controls/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/transport-controls/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/transport-controls/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.midi-pattern-laboratory",
        name: "MIDI Pattern Laboratory",
        summary: "Author-written bounded MIDI patterns on isolated native internal voices, with cancellation and honest state inspection.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/midi-pattern-laboratory/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/midi-pattern-laboratory/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/midi-pattern-laboratory/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.editor-overlay-laboratory",
        name: "Editor Overlay Laboratory",
        summary: "Lua-authored retained checker/image-mask guides and captured Player transport readouts; no musical edits.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/editor-overlay-laboratory/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/editor-overlay-laboratory/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/editor-overlay-laboratory/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.image-art-studio",
        name: "Image Art Studio",
        summary: "Lua-authored captured-image note artwork and native mask editing with Preview/Undo and bounded source presets.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/image-art-studio/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/image-art-studio/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/image-art-studio/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.text-art-studio",
        name: "Text Art Studio",
        summary: "Lua-authored original glyph artwork and multiline note composition with native Preview, presets and Undo.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/text-art-studio/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/text-art-studio/main.lua"),
            ),
            (
                "color-laboratory.lua",
                include_str!("../../examples-lua/text-art-studio/color-laboratory.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/text-art-studio/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.note-exchange",
        name: "Note Exchange",
        summary: "Author-written streaming note format, native import review/Undo and atomic export.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/note-exchange/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/note-exchange/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/note-exchange/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.midi-input-laboratory",
        name: "MIDI Input Laboratory",
        summary: "Explicit device capture, persisted replay and Lua-authored note/CC/Pitch Bend artwork with native Preview/Undo.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/midi-input-laboratory/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/midi-input-laboratory/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/midi-input-laboratory/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.font-resources",
        name: "Font Resource Inspector",
        summary: "Native author-written paged installed-font chooser, scalar outline canvas, plain presets and resource inspection.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/font-resources/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/font-resources/main.lua"),
            ),
            (
                "font-picker.lua",
                include_str!("../../examples-lua/font-resources/font-picker.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/font-resources/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.file-transform",
        name: "Selected Text File",
        summary: "Explicit selected-file streaming transform and atomic selected-output save; no unrestricted file access.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/file-transform/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/file-transform/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/file-transform/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.geometry-primitives",
        name: "Geometry Primitives",
        summary: "Authored path, curve, shape and mask algorithms composed with bounded native geometry and note transactions.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/geometry-primitives/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/geometry-primitives/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/geometry-primitives/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.metadata-notebook",
        name: "Metadata Notebook",
        summary: "Author-owned sparse project, Track and Group attributes with schema migration, native Undo and LUP persistence.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/metadata-notebook/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/metadata-notebook/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/metadata-notebook/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.note-service",
        name: "Composable Note Service",
        summary: "Pure bounded author-written pattern and packed-note transform services; separately installed from callers.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/note-service-provider/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/note-service-provider/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/note-service-provider/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.service-composer",
        name: "Composable Services Laboratory",
        summary: "Explicit provider dependencies, seeded pure data calls and atomic native musical transactions.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/service-composer/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/service-composer/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/service-composer/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.reactive-monitor",
        name: "Reactive Project Monitor",
        summary: "Read-only bounded typed project observation and author-owned recent-event history.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/reactive-project-monitor/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/reactive-project-monitor/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/reactive-project-monitor/README.md"),
            ),
        ],
    },
    Example {
        id: "plumspirit.example.reactive-velocity",
        name: "Reactive Velocity Guard",
        summary: "Separately authorized bounded automatic note responses, native Undo and causal-loop suppression.",
        files: &[
            (
                "plum-plugin.toml",
                include_str!("../../examples-lua/reactive-velocity-guard/plum-plugin.toml"),
            ),
            (
                "main.lua",
                include_str!("../../examples-lua/reactive-velocity-guard/main.lua"),
            ),
            (
                "README.md",
                include_str!("../../examples-lua/reactive-velocity-guard/README.md"),
            ),
        ],
    },
];

/// The example with manifest id `id`.
pub fn by_id(id: &str) -> Option<&'static Example> {
    EXAMPLES.iter().find(|e| e.id == id)
}

impl Example {
    /// The entry script's text.
    pub fn main_lua(&self) -> &'static str {
        self.files
            .iter()
            .find(|(p, _)| *p == "main.lua")
            .map_or("", |(_, t)| t)
    }

    /// Writes the package into a new folder under `parent` and returns it
    /// (an existing folder of that name is replaced).
    pub fn write_to(&self, parent: &Path) -> Result<PathBuf, String> {
        let dir = parent.join(self.id);
        let fail = |e: std::io::Error| {
            format!(
                "The example {} could not be prepared in {}: {e}.",
                self.name,
                parent.display()
            )
        };
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(fail)?;
        }
        std::fs::create_dir_all(&dir).map_err(fail)?;
        for (rel, text) in self.files {
            std::fs::write(dir.join(rel), text).map_err(fail)?;
        }
        Ok(dir)
    }
}
