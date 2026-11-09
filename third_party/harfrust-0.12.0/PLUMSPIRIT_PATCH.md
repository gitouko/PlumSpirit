# PlumSpirit native shaping patch

Exact harfrust0.12.0 registry source already locked by this workspace, MIT (LICENSE).
Only source change: additive GlyphBuffer::successful() reads the existing native
successful flag. No shaping algorithm, cache, operation limit or result changes.
The host needs to refuse partial output when untrusted fonts exhaust native budgets.
External font cache/runtime allocation is separately isolated in a bounded process.
Cargo manifest sources are the registry-normalized upstream files; no network update.
