//! Prints the transform tool registry as tab-separated lines (v0.9 §096, §101):
//! `TOOL id name family params`, then `ADV index name params` per advanced
//! function. `tools/catalog_sync.py` turns it into / checks it against
//! `docs/v0_9/TOOL_CATALOG.toml`.
//!
//! ```text
//! cargo run -p plumspirit_engine --example tool_catalog
//! ```
use plumspirit_engine::transform::registry::registry;

fn main() {
    for t in registry() {
        let s = (t.spec)();
        let base = s.params.iter().filter(|p| p.adv == 0).count();
        println!("TOOL\t{}\t{}\t{:?}\t{}", s.id, s.name, s.family, base);
        for (i, a) in s.advanced.iter().enumerate() {
            let n = s
                .params
                .iter()
                .filter(|p| usize::from(p.adv) == i + 1)
                .count();
            println!("ADV\t{}\t{}\t{}", i + 1, a, n);
        }
    }
}
