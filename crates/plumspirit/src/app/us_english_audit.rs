//! B1-0067: every user-facing string literal of the app and the engine is in
//! American English. Literals that are bare lowercase keys / ids (settings
//! keys, parameter ids such as `a4.centres`, Spiderweb state keys) are kept
//! for compatibility with stored files and presets; the Tumours tool keeps
//! the owner's name. `tools/us_english.py fix` converts new strings.

/// British spellings that have an American form (lowercase stems).
const BRITISH: &[&str] = &[
    "colour",
    "centre",
    "centred",
    "centring",
    "neighbour",
    "behaviour",
    "analyse",
    "analysing",
    "licence",
    "travelling",
    "travelled",
    "cancelled",
    "cancelling",
    "labelled",
    "labelling",
    "modelling",
    "favourite",
    "honour",
    "catalogue",
    "dialogue",
    "grey",
    "normalis",
    "quantis",
    "optimis",
    "customis",
    "initialis",
    "recognis",
    "synchronis",
    "visualis",
    "organis",
    "randomis",
    "summaris",
    "maximis",
    "minimis",
];

/// The contents of every plain string literal in Rust source `src`, with
/// its line (comments, char literals and raw / byte strings skipped).
pub(super) fn literals(src: &str) -> Vec<(usize, String)> {
    let b = src.as_bytes();
    let (mut i, n) = (0usize, b.len());
    let mut out = Vec::new();
    let ident = |k: usize| k > 0 && (b[k - 1].is_ascii_alphanumeric() || b[k - 1] == b'_');
    while i < n {
        if b[i..].starts_with(b"//") {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if b[i] == b'r' && !ident(i) && i + 1 < n && (b[i + 1] == b'"' || b[i + 1] == b'#') {
            let mut j = i + 1;
            let mut hashes = 0;
            while j < n && b[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && b[j] == b'"' {
                let close: Vec<u8> = std::iter::once(b'"')
                    .chain(std::iter::repeat_n(b'#', hashes))
                    .collect();
                j += 1;
                while j < n && !b[j..].starts_with(&close) {
                    j += 1;
                }
                i = j + close.len();
            } else {
                i += 1;
            }
        } else if b[i] == b'b' && !ident(i) && i + 1 < n && b[i + 1] == b'"' {
            let mut j = i + 2;
            while j < n && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            i = j + 1;
        } else if b[i] == b'\'' {
            // A char literal ('x', '\n', '"', '\u{..}') or a lifetime.
            if i + 2 < n && b[i + 1] == b'\\' {
                let mut j = i + 2;
                while j < n && b[j] != b'\'' {
                    j += 1;
                }
                i = j + 1;
            } else if i + 2 < n && b[i + 2] == b'\'' {
                i += 3;
            } else {
                // A multi-byte char: skip to its closing quote if close by.
                let end = (i + 6).min(n);
                i = match b[i + 1..end].iter().position(|&c| c == b'\'') {
                    Some(p) if !src[i + 1..i + 1 + p].contains(char::is_whitespace) => i + 2 + p,
                    _ => i + 1,
                };
            }
        } else if b[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < n && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            let line = src[..start].matches('\n').count() + 1;
            out.push((line, src[start..j.min(n)].to_owned()));
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// A bare lowercase key / id (kept for stored files and presets).
fn is_key(lit: &str) -> bool {
    lit.bytes()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.:/-".contains(&c))
}

/// Stems that are British inside a word too (multicolour, epicentre).
const INNER: &[&str] = &[
    "colour",
    "centre",
    "centred",
    "neighbour",
    "behaviour",
    "favourite",
];

fn british(lit: &str) -> Option<&'static str> {
    let low = lit.to_lowercase();
    BRITISH.iter().copied().find(|w| {
        low.match_indices(w).any(|(k, _)| {
            INNER.contains(w) || k == 0 || !low.as_bytes()[k - 1].is_ascii_alphabetic()
        })
    })
}

pub(super) fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).expect("source directory").flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn the_lexer_finds_literals_and_skips_comments_and_chars() {
    let src = "let a = \"Colour\"; // \"Centre\"\nlet c = '\"'; let d = \"x \\\" y\";\n/* \"grey\" */ r#\"Behaviour\"#; b\"colour\"; 'a: loop {}";
    let got: Vec<String> = literals(src).into_iter().map(|l| l.1).collect();
    assert_eq!(got, ["Colour", "x \\\" y"]);
    assert!(is_key("a4.centres") && !is_key("Center X") && !is_key("the colour"));
    assert_eq!(british("Map behaviour"), Some("behaviour"));
    assert_eq!(british("Background"), None);
    assert_eq!(british("Tumours"), None);
    assert_eq!(british("multicolour keys"), Some("colour"));
    assert_eq!(british("greyhound"), Some("grey"));
}

/// B1-0067 guard: no British spelling in a user-facing string literal of
/// `crates/plumspirit/src` or `crates/plumspirit_engine/src` (test code
/// included: what it asserts is what users see).
#[test]
fn user_facing_strings_are_american_english() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("../plumspirit_engine/src"), &mut files);
    assert!(files.len() > 100, "found the sources");
    let mut found = Vec::new();
    for f in files {
        if f.ends_with("us_english_audit.rs") {
            continue;
        }
        let src = std::fs::read_to_string(&f).expect("readable source");
        for (line, lit) in literals(&src) {
            if is_key(&lit) {
                continue;
            }
            if let Some(w) = british(&lit) {
                found.push(format!("{}:{line}: {w}: {lit}", f.display()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "{} British spelling(s) (run tools/us_english.py fix):\n{}",
        found.len(),
        found.join("\n")
    );
}

/// B1-0066 guard: no tooltip or label hard-codes a keyboard shortcut that the
/// user can rebind — the default chord of a bindable command (with a
/// modifier or a named key) never appears in a string literal of the app;
/// tooltips take it from `keybinds::hint` / `Command::shortcut`, which follow
/// the user's keybinds. Mouse gestures (Ctrl+click, Shift+drag, Ctrl+wheel)
/// are not keybinds and stay as text.
#[test]
fn no_string_hard_codes_a_rebindable_shortcut() {
    use super::keybinds::{KEYBINDS, chord_text};
    // Chords with a modifier anywhere; plain keys (F6, Home, Space) only as
    // a "(key)" hint, so the words "End" or "Delete" are not taken for keys.
    let mut chords: Vec<String> = KEYBINDS
        .iter()
        .map(|k| {
            let c = chord_text(k.mods, k.key);
            if k.mods.is_none() {
                format!("({c})")
            } else {
                c
            }
        })
        .filter(|c| c.contains('+') || c.len() > 3)
        .collect();
    chords.sort_by_key(|c| std::cmp::Reverse(c.len()));
    chords.dedup();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut found = Vec::new();
    for f in files {
        let name = f.to_string_lossy();
        // The keybind table itself, the help window that lists it, and tests.
        // Spiderweb keeps its own board keys (not in the keybind table).
        if name.ends_with("keybinds.rs")
            || name.ends_with("_audit.rs")
            || name.contains("test")
            || name.ends_with("_matrix.rs")
            || name.contains("spiderweb")
        {
            continue;
        }
        let src = std::fs::read_to_string(&f).expect("readable source");
        // Test modules at the end of a file are not user-facing.
        let src = src.split("#[cfg(test)]").next().unwrap_or("");
        for (line, lit) in literals(src) {
            // The Event Graph's Enter applies the anchors; it is not a
            // keybind (the table's Enter opens a clip).
            if lit.starts_with("Write the curve between the anchors") {
                continue;
            }
            for c in &chords {
                let hit = lit.match_indices(c.as_str()).any(|(k, _)| {
                    let before = lit[..k].chars().next_back();
                    let after = lit[k + c.len()..].chars().next();
                    !before.is_some_and(|b| b.is_alphanumeric() || b == '+')
                        && !after.is_some_and(|a| a.is_alphanumeric() || a == '+')
                });
                if hit {
                    found.push(format!("{}:{line}: {c}: {lit}", f.display()));
                    break;
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "{} hard-coded shortcut(s) (use keybinds::hint):\n{}",
        found.len(),
        found.join("\n")
    );
}
