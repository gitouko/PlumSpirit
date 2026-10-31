# PlumSpirit v1.0.0 — Portable ZIP static release checks

Validated on: 2026-10-31 (packaging audit, **not** live Windows execution).

## Confirmed by static inspection

- The uploaded SoundFont ZIP passes CRC/integrity testing and matches the delivery JSON SHA-256.
- `GeneralUser-GS.sf2` file SHA-256: `9575028c7a1f589f5770fccc8cff2734566af40cd26ed836944e9a5152688cfe`.
- SoundFont bytes: `32,319,396`; valid RIFF `sfbk` header with matching RIFF size.
- Bundled SoundFont license and third-party notices retained.
- Original `PlumSpirit.exe` is byte-for-byte unchanged; SHA-256: `1ef1365d92bb60b654581f08f95ca61e31e889709330f3760a0a3f327bceab70`.
- The app's source has a `bundled_soundfont()` lookup for `SoundFonts/GeneralUser-GS.sf2` beside the EXE.
- Prepared portable ZIP passed ZIP CRC test and included all required docs and the SF2.

## Not verified by this packaging audit

- Running `PlumSpirit.exe` in a real Windows 10/11 x64 session.
- Successful SoundFont playback, actual audio compatibility and Windows device switching.
- Full GUI/editor import-export tests, especially Kiva/Piano From Above interoperability.
- Code signing, Windows SmartScreen reputation, antivirus scan on the actual user's machine.
- Reproducible matching of executable to the source snapshot: delivery metadata lists `exe_built_from` `b2c5093e56bf0284b4e4b3b5796bd9897394d70a` but `source_commit` `62dfc7e3936701fd9aa2bfc633938d05f861cbc8`. The difference requires owner/build-system confirmation before claiming byte-for-byte source-to-binary provenance.
- Ownership/licensing review of all original source and branding; MIT license is publisher-selected, but permissions must be confirmed.

**Verdict:** ZIP and SoundFont integrity verified. Live Windows acceptance and source-to-EXE provenance still require independent verification before claiming a fully tested public release.
