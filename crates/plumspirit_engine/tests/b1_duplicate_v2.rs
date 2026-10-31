//! B1-S44 (contract §013-E, B1-0099): "Paste Repeat (Notes)" is Duplicate V2
//! in the Iterations section. The id `paste_repeat_notes` is stable: presets
//! written before the rename load, its category (and so its parameters and
//! ten functions) is unchanged, and the Track List's Paste Repeat is a
//! separate tool that keeps its name.

use plumspirit_engine::transform::definition::{Category, definition};
use plumspirit_engine::transform::preset;
use plumspirit_engine::transform::registry::{Family, registry, tool};
use plumspirit_engine::transform::{ParamSet, ParamValue};

#[test]
fn duplicate_v2_is_the_old_paste_repeat_notes_under_iterations() {
    let def = tool("paste_repeat_notes").expect("the old id still resolves");
    let spec = (def.spec)();
    assert_eq!(spec.name, "Duplicate V2");
    assert_eq!(spec.family, Family::Iterations);
    assert_eq!(spec.advanced.len(), 10, "its ten functions are kept");
    let d = definition(def);
    assert_eq!(
        d.category,
        Category::PasteRepeat,
        "no mandatory functions added"
    );
    assert_eq!(d.category.key(), "paste-repeat");
    // No tool is called Paste Repeat (notes) any more, and no registered
    // tool is left in the old Paste Repeat family.
    for t in registry() {
        let s = (t.spec)();
        assert!(!s.name.contains("Paste Repeat"), "{}: {}", t.id, s.name);
        assert_ne!(s.family, Family::PasteRepeat, "{}", t.id);
    }
}

#[test]
fn a_preset_saved_before_the_rename_loads_into_duplicate_v2() {
    let def = tool("paste_repeat_notes").unwrap();
    let spec = (def.spec)();
    let old =
        "PlumSpirit Preset 1\ntool=paste_repeat_notes\nname=Four copies\np.count=4\np.spacing=1\n";
    let (p, skipped) = preset::parse(old, def.id, &spec.params).expect("the old preset loads");
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(p.tool, "paste_repeat_notes");
    assert_eq!(p.values.get_raw("count"), Some(&ParamValue::Int(4)));
    assert_eq!(p.values.get_raw("spacing"), Some(&ParamValue::Choice(1)));
    // Saved again, it is written under the same id.
    let text = preset::to_text(&p);
    assert!(text.contains("tool=paste_repeat_notes"), "{text}");
    let _: &ParamSet = &p.values;
}
