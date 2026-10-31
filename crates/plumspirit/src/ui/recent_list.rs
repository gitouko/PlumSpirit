//! Recent Files lists (v0.8 §076): the File → Recent Files submenu and the
//! start screen's list. Both only return what was asked for; the app applies
//! it (the list never touches the files themselves).

use crate::recent::{RecentAction, RecentEntry, age_text};
use crate::ui::theme::{Palette, size};
use egui::{Button, RichText};

/// Hover text of an entry: path, type, when it was opened and read.
pub fn entry_hover(e: &RecentEntry, missing: bool, now: u64) -> String {
    let mut s = format!("{}\n{}", e.path.display(), e.kind.label());
    if e.last_opened > 0 {
        s.push_str(&format!(" · opened {}", age_text(now, e.last_opened)));
    }
    match e.last_success {
        Some(t) if t != e.last_opened => {
            s.push_str(&format!(" · last read or saved {}", age_text(now, t)))
        }
        None => s.push_str(" · never opened successfully"),
        _ => {}
    }
    if e.pinned {
        s.push_str(" · pinned");
    }
    if missing {
        s.push_str("\nThe file is missing (moved, renamed or deleted). Click to locate it.");
    }
    s
}

fn entry_label(e: &RecentEntry, missing: bool) -> RichText {
    let t = format!("{}{}", e.name, if missing { "  (missing)" } else { "" });
    if missing {
        RichText::new(t).weak()
    } else {
        RichText::new(t)
    }
}

fn open_or_locate(e: &RecentEntry, missing: bool) -> RecentAction {
    if missing {
        RecentAction::Locate(e.path.clone())
    } else {
        RecentAction::Open(e.path.clone())
    }
}

/// The contents of File → Recent Files (submenus only, no popups inside the
/// menu: egui keeps one popup open at a time).
pub fn menu_items(
    ui: &mut egui::Ui,
    entries: &[RecentEntry],
    missing: &[bool],
    now: u64,
) -> Option<RecentAction> {
    let mut act = None;
    if entries.is_empty() {
        ui.add_enabled(false, Button::new("No recent files"));
        return None;
    }
    let is_missing = |i: usize| missing.get(i).copied().unwrap_or(false);
    let mut was_pinned = entries.first().is_some_and(|e| e.pinned);
    if was_pinned {
        ui.label(RichText::new("Pinned").small().weak());
    }
    for (i, e) in entries.iter().enumerate() {
        if was_pinned && !e.pinned {
            ui.separator();
            was_pinned = false;
        }
        if ui
            .add(Button::new(entry_label(e, is_missing(i))))
            .on_hover_text(entry_hover(e, is_missing(i), now))
            .clicked()
        {
            act = Some(open_or_locate(e, is_missing(i)));
            ui.close();
        }
    }
    ui.separator();
    ui.menu_button("Pin / Unpin", |ui| {
        for e in entries {
            let mut on = e.pinned;
            if ui.checkbox(&mut on, e.name.as_str()).clicked() {
                // L-F05: a check toggles; the menu stays open.
                act = Some(RecentAction::Pin(e.path.clone(), on));
            }
        }
    });
    ui.menu_button("Remove from History", |ui| {
        for (i, e) in entries.iter().enumerate() {
            if ui
                .add(Button::new(entry_label(e, is_missing(i))))
                .on_hover_text("Removes it from this list only; the file is not touched.")
                .clicked()
            {
                act = Some(RecentAction::Remove(e.path.clone()));
                ui.close();
            }
        }
    });
    let n_missing = missing.iter().filter(|&&m| m).count();
    if ui
        .add_enabled(
            n_missing > 0,
            Button::new(format!("Clear Missing ({n_missing})")),
        )
        .on_hover_text("Removes the entries whose file no longer exists (pinned ones too).")
        .clicked()
    {
        act = Some(RecentAction::ClearMissing);
        ui.close();
    }
    if ui
        .add_enabled(
            entries.iter().any(|e| !e.pinned),
            Button::new("Clear History"),
        )
        .on_hover_text("Removes every unpinned entry from this list; no file is touched.")
        .clicked()
    {
        act = Some(RecentAction::Clear);
        ui.close();
    }
    act
}

/// Entries shown on the start screen.
pub const START_SCREEN_ENTRIES: usize = 8;

/// The start screen's "Recent" list (click opens; right-click: Open, Pin /
/// Unpin, Locate…, Remove from History).
pub fn start_list(
    ui: &mut egui::Ui,
    pal: &Palette,
    entries: &[RecentEntry],
    missing: &[bool],
    now: u64,
) -> Option<RecentAction> {
    let mut act = None;
    if entries.is_empty() {
        return None;
    }
    ui.label(
        RichText::new("Recent")
            .size(size::SMALL)
            .color(pal.text_weak),
    );
    ui.add_space(4.0);
    for (i, e) in entries.iter().take(START_SCREEN_ENTRIES).enumerate() {
        let m = missing.get(i).copied().unwrap_or(false);
        let mut label = entry_label(e, m);
        if e.pinned {
            label = label.strong();
        }
        let r = ui
            .add(Button::new(label).frame(false))
            .on_hover_text(entry_hover(e, m, now));
        if r.clicked() {
            act = Some(open_or_locate(e, m));
        }
        r.context_menu(|ui| {
            if ui.add_enabled(!m, Button::new("Open")).clicked() {
                act = Some(RecentAction::Open(e.path.clone()));
                ui.close();
            }
            if ui.button("Locate…").clicked() {
                act = Some(RecentAction::Locate(e.path.clone()));
                ui.close();
            }
            if ui.button(if e.pinned { "Unpin" } else { "Pin" }).clicked() {
                act = Some(RecentAction::Pin(e.path.clone(), !e.pinned));
                ui.close();
            }
            if ui.button("Remove from History").clicked() {
                act = Some(RecentAction::Remove(e.path.clone()));
                ui.close();
            }
        });
    }
    if entries.len() > START_SCREEN_ENTRIES {
        ui.label(
            RichText::new(format!(
                "{} more in File → Recent Files",
                entries.len() - START_SCREEN_ENTRIES
            ))
            .size(size::SMALL)
            .color(pal.text_weak),
        );
    }
    act
}
