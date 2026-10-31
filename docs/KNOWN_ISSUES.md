# Known setup limitations for this archive

- GeneralUser GS v2.0.3 is bundled. If internal playback is silent, check your selected SoundFont and audio device; a project-specific/custom bank may override the bundled default.
- OmniMIDI/KDMAPI is an external Windows MIDI setup; it is not provided by this ZIP.
- Settings live in `%APPDATA%\PlumSpirit`, even though the EXE requires no installer.
- The EXE is not code-signed; Microsoft SmartScreen may display a warning.
- Extremely dense MIDI projects can exceed low-end memory/CPU capabilities.
- The current ZIP passed static checks, but no independent Windows playback or UI tests were performed during repackaging.
- Please report reproducible issues with version, reproduction steps, Windows build, and a small MIDI if public sharing is acceptable.
