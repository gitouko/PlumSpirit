# Getting started with PlumSpirit

PlumSpirit is a Windows x64 portable app. Download the Windows Portable ZIP from the latest verified GitHub Release, extract it as a folder, and open `PlumSpirit.exe`. You don't need a Rust development environment.

## Your first MIDI

Open PlumSpirit, choose New Project, give it a name, and set the starting tempo and PPQN. Create a musical Track, then add/open a Piano Roll Clip to place notes. Save the editable project as `.lup`; export as `.mid` when you want a file that other MIDI players can read.

## Playing it back

GeneralUser GS v2.0.3 is included in `SoundFonts/`, and PlumSpirit can use it automatically if no custom SoundFont is selected. You can choose a different SF2 from audio settings if you prefer. External MIDI outputs are optional and depend on what's installed on your computer. For OmniMIDI/KDMAPI, install and configure OmniMIDI separately.

## Where to look next

* Help → Introduction / Quick Start for the main windows and tools.
* The Tool sections for transforms and note generators. Most editing tools have Preview.
* The MIDI Inspector for note counts, NPS/polyphony and project timing.
* [`docs/LUA_API.md`](LUA_API.md) for creating your own Lua tools.

For bugs, use GitHub Issues and include the PlumSpirit version, steps to reproduce, and (if you want) a small sample MIDI. Don't upload private source material without permission.
