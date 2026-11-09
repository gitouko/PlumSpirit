# Selected Text File — API 0.2 example

Install this folder through Plugins → Lua Plugins → Install. Grant only the two
requested user-selected file permissions and enable it. Under Plugins → Selected
Text File, run Uppercase ASCII in UTF-8 Text. Select an input and an output.

The Lua author supplies the algorithm: ASCII a–z becomes A–Z; other UTF-8 bytes,
binary newline spelling and optional UTF-8 BOM stay unchanged. CSV/JSON are treated
as UTF-8 text; this example does not parse or promise to preserve their semantics.
Invalid/incomplete UTF-8 fails without publishing staged output. Memory stays bounded
at 64 KiB chunks plus at most three carried bytes, subject to normal VM/native quotas.
Cancellation or returning without commit discards staging. A completed commit is a
filesystem save, outside project Undo; a later script error cannot undo that save.

Acceptance: cancel Open; cancel Save; transform empty/ASCII/accented text and a code
point crossing byte 65,536; reject invalid UTF-8; compare exact bytes/newlines; fail
midway and verify original destination; copy a large input without a full Lua string.
Automated algorithm/I/O coverage: lua::files::tests. Real native dialogs have a
separate Windows acceptance record in docs/v1_0_1_api02/EVIDENCE.jsonl.

This is part of an in-development expansion. The complete v1.0.1 release gates are
still open; do not infer release completion from the sample's API requirement.


This package is also embedded in **Plugins > Lua Plugins > Examples**; no download
or source checkout is needed. Installation starts disabled with no capabilities
granted by default. Review the listed permissions and enable it using the normal
plugin controls. Reinstall restores the shipped files and preserves your decisions.
