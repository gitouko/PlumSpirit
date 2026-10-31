//! B1-0066: every tool control has a tooltip — the help text of its
//! parameter, written from its kernel. Was a ratchet (1,866 controls without
//! help at B1-S33 part 5); since part 6 none may be without help, and every
//! help text is a short, complete sentence that says more than the label.

use plumspirit_engine::transform::definition::definitions;
use std::collections::BTreeMap;

#[test]
fn every_tool_control_has_help_text() {
    let mut missing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let (mut total, mut without) = (0usize, 0usize);
    let mut bad = Vec::new();
    for d in definitions() {
        for p in &d.params {
            total += 1;
            let h = p.spec.help.trim();
            if h.is_empty() {
                without += 1;
                missing.entry(d.id).or_default().push(p.spec.key);
                continue;
            }
            let ends = h.ends_with('.') || h.ends_with('?') || h.ends_with(')') || h.ends_with('…');
            if !ends || h.eq_ignore_ascii_case(p.spec.label) || h.chars().count() > 400 {
                bad.push(format!("{} {}: {h:?}", d.id, p.spec.key));
            }
        }
    }
    let mut by_tool: Vec<(&str, usize)> = missing.iter().map(|(t, v)| (*t, v.len())).collect();
    by_tool.sort_by_key(|x| std::cmp::Reverse(x.1));
    println!(
        "{without} of {total} controls without help; most: {:?}",
        &by_tool[..by_tool.len().min(25)]
    );
    let mut keys: BTreeMap<&str, usize> = BTreeMap::new();
    for v in missing.values() {
        for k in v {
            *keys.entry(k).or_default() += 1;
        }
    }
    let shared: usize = keys.values().filter(|&&n| n > 3).count();
    let shared_hits: usize = keys.values().filter(|&&n| n > 3).sum();
    println!(
        "unique keys without help: {}; shared by > 3 tools: {shared} keys covering {shared_hits} controls",
        keys.len()
    );
    let mut common: Vec<_> = keys.iter().filter(|(_, n)| **n > 3).collect();
    common.sort_by_key(|x| std::cmp::Reverse(*x.1));
    println!("{:?}", &common[..common.len().min(60)]);
    assert!(
        bad.is_empty(),
        "help texts that are not sentences:\n{}",
        bad.join("\n")
    );
    assert_eq!(
        without, 0,
        "{without} controls without help: every control needs a tooltip (B1-0066)"
    );
}

/// Writing aid: `PLUMSPIRIT_HELP_TOOL=note_props cargo test -p
/// plumspirit_engine --test b1_tooltips missing -- --ignored --nocapture`
/// lists the tool's controls without help (key, label, function).
#[test]
#[ignore = "writing aid"]
fn missing_help_of_one_tool() {
    let Ok(id) = std::env::var("PLUMSPIRIT_HELP_TOOL") else {
        return;
    };
    for want in id.split(',') {
        let Some(d) = definitions().into_iter().find(|d| d.id == want) else {
            println!("no tool {want}");
            continue;
        };
        for p in &d.params {
            if p.spec.help.trim().is_empty() {
                let f = d
                    .functions
                    .iter()
                    .find(|f| f.n == p.spec.adv)
                    .map_or("", |f| f.name);
                println!("{want}\t{}\t{}\t{f}", p.spec.key, p.spec.label);
            }
        }
    }
}
