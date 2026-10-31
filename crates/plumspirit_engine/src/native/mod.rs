//! PlumSpirit's native project format (`.lup`).

pub mod lup;

#[cfg(test)]
mod clip_tests;

pub use lup::{
    LUP_EXTENSION, LUP_MAGIC, LUP_VERSION, LUP_VERSION_256, LUP_VERSION_CLIPS, LupControl,
    LupError, LupLoadReport, LupWriteStats, SaveReport, SaveStage, has_lup_signature, load_project,
    project_from_bytes, project_to_bytes, read_project, save_project, verify_project,
    write_project,
};
