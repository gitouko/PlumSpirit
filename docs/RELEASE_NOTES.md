# PlumSpirit v1.0.0 — First public release

PlumSpirit has finally reached its first public release. This is a free, portable Black MIDI sequencer and editor for Windows x64.

You can make a project from scratch, edit existing MIDIs, use the Track List and Piano Roll, work with multiple Tracks and Subchannels, and try a huge collection of experimental note tools and generators. There's also a MIDI Player, project analysis/Inspector, SoundFont playback, and a public Lua API for community-created tools.

**A note about development:** PlumSpirit was vibe-coded with AI assistance. Reaching v1.0 involved 33 days of planning, reviewing, testing and adjusting the software. That doesn't mean every possible bug has been found. Please report issues you run into so they can be addressed in later versions.

## Download

Use the release asset named `PlumSpirit-v1.0.0-Windows-x64-Portable.zip` **after the publisher's final Windows acceptance check**. Extract it and run `PlumSpirit.exe`. No installer or development environment is required. The portable ZIP includes `PlumSpirit.exe`, GeneralUser GS v2.0.3 (`SoundFonts/GeneralUser-GS.sf2`), Lua plugin examples, documentation, and third-party licenses.

**Release approval:** The portable ZIP has passed static integrity checks, but actual Windows playtesting and source-to-binary provenance must be confirmed by the publisher before making a "tested" claim.

## Source and permissions

The original PlumSpirit source is MIT licensed. You can make Black MIDI with PlumSpirit without crediting the program. Forks of the code must retain the relevant license notices. Third-party components and sample banks keep their own licenses.

## Known limitations

Performance depends strongly on the size and density of a MIDI and the machine running it. Very large projects can exceed low-memory hardware limits. Some archived-file formats and some external MIDI-output configurations require additional setup. Details belong in the README and current known-limitations document.

Linux and Android are planned for later; macOS is tentatively scheduled for late 2027. None of these are v1.0 Windows downloads.

Thanks to the Black MIDI community for the feedback and testing that helped get PlumSpirit this far.
