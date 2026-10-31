//! Standard MIDI File reading and writing.
//!
//! Parser → owned document → exporter. The document never keeps references into the
//! source file, and the source file is never modified.

pub mod compare;
pub mod error;
pub mod export;
pub mod import;
pub mod merge;
pub mod overlap;
pub mod plan;
pub mod provenance;
pub mod reader;
pub mod semantic;
pub mod seqspec;
pub mod snapshot;
pub mod strict;
pub mod structure;
#[cfg(test)]
pub(crate) mod test_smf;
pub mod validate;
pub mod vlq;
pub(crate) mod workers;

pub use error::{ImportWarning, MidiError, MidiErrorKind, group_digits};
pub use export::{
    ExportControl, ExportError, ExportReport, ExportStage, WriteStats, export_to_bytes,
    export_to_bytes_with, export_to_path, write_document,
};
pub use import::{
    ImportControl, ImportStage, ImportStages, ImportSummary, import_bytes, import_bytes_with,
    import_file, import_file_with, import_reader, import_reader_with,
};
pub use overlap::{SameKeyOverlaps, same_key_disclosure};
pub use plan::{
    ExportOptions, ExportPlan, LossyTempo, PlanEvent, PlanKind, Rescale, TempoReport,
    WideKeyReport, WideKeys, plan_export, plan_export_with,
};
pub use snapshot::{ExportSnapshot, SnapshotNote};
pub use validate::{
    ValidationReport, validate_exported_bytes, validate_exported_file, validate_written_bytes,
    validate_written_file,
};
