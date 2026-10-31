# PlumSpirit v1.0 — Start Here

**PlumSpirit is a portable Windows x64 Black MIDI sequencer.** You don't need to install Rust, build anything, or run an installer.

1. Extract the **entire ZIP** to a normal folder (for example, in Documents). Do not launch the EXE from inside the ZIP.
2. Double-click `PlumSpirit.exe`.
3. **Audio is ready to configure:** `SoundFonts/GeneralUser-GS.sf2` is included with the release, with its separate license. PlumSpirit automatically looks for this bank when no other SoundFont has been selected. If another SoundFont is already selected in your project or settings, it may take priority; choose GeneralUser GS through audio settings if necessary. The bank is approximately 31 MiB, so allow for audio memory usage.
4. Create or open a project. PlumSpirit projects use `.lup` and exported standard MIDI files use `.mid`. Open a Clip in the Piano Roll to edit notes. F1 opens Help.
5. For problems, see `docs/KNOWN_ISSUES.md` and report a reproducible bug through the official GitHub repository Issues page. Include a crash log/diagnostic if possible, but do not share private MIDIs without permission.

**No credit required for music you make using PlumSpirit.** Code forks and redistribution follow the MIT license (`LICENSE`) and applicable third-party notices (`legal/`).

Windows might show an unrecognized-app warning for unsigned software. Only run downloads from sources you trust. No Windows SmartScreen or antivirus reputation clearance is claimed here.

Settings may be stored under `%APPDATA%\PlumSpirit`; "portable" refers to not needing an installer and does not mean all settings stay in the extracted folder.

For the developer's 33-day story and the honest vibe-coding disclosure, read `README.md`.
