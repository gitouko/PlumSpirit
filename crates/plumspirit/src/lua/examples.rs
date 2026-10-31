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
pub const EXAMPLES: [Example; 2] = [
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
