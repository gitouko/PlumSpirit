//! UI building blocks (panels, theme, fonts). They render state and return user
//! intents; they never own document data.

pub mod empty_state;
pub mod fonts;
pub mod graph_bar;
pub mod icons;
pub mod menu;
pub mod numeric;
pub mod onion_chooser;
pub mod param_editors;
pub mod ranges_editor;
pub mod recent_list;
pub mod status_bar;
pub mod stepper;
#[cfg(test)]
pub mod testkit;
pub mod theme;
pub mod toolbar;
pub mod track_panel;
pub mod transport_bar;
