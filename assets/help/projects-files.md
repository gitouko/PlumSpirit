# Projects and Files

## Project files (.lup)

A PlumSpirit project is a `.lup` file. It keeps everything exactly: every note and MIDI
event, the Conductor (exact tempo points and ramps, time signatures, markers and
chapters), the PPQN, tracks, subtracks, folders, subchannels, clips, note groups and muted
notes, Mute / Solo, the loop, Onion Skin and Event Graph settings, the Track List view and
the project's SoundFont.

* {cmd:NewProject} — a name, a tempo and a PPQN (ticks per quarter note); 4/4 and one
  track. **Settings → Import / Export** sets what the dialog starts with (BPM, PPQN, 256-Key Mode).
* {cmd:Open} — a `.lup` project, or a MIDI file to import. The MIDI file is only read.
  The status bar names the stage of a long import (reading, decoding the last tracks,
  assembling, checking the project) with the time it took so far; it reaches 100 % only when
  the project is open, and Esc or the ✕ cancels it.
* {cmd:Save} — saves the `.lup`. A new project, or one imported from MIDI, asks for a
  `.lup` name: Save never overwrites a `.mid`.
* {cmd:SaveAs} — saves under a new name.
* {cmd:ProjectProperties} — rename the project, change its tempo, turn **256-Key Mode** on
  or off, and **Change PPQN (Experimental)…** (below).
* {cmd:CloseProject} — back to the start screen.

The window title shows `*` while there are unsaved changes. Closing or opening another
file then asks Save, Don't Save or Cancel. Saving is transactional: PlumSpirit writes a
temporary file, checks it and only then replaces the old one, so a failed save never
damages your project.

Close Project releases its notes, clips, analysis, view caches and audio data in the
background. **Releasing project memory…** appears while cleanup finishes; the start
screen remains usable. Project clipboards and selection slots are cleared. Saved files,
persistent project history, SoundFonts, the audio device and application settings remain.
The completion message's Details shows a project-data estimate and process memory.
Windows and the allocator can retain freed pages for reuse, so process RAM need not
immediately return to its starting value.

## Recent files and backups

**File → Recent Files** and the start screen list what you opened. **Pin** keeps an entry
at the top; a moved file shows **Locate…**; removing entries never touches your files.

**Automatic Backup** (Settings → Import / Export): while a project has unsaved changes,
PlumSpirit writes a verified copy every few minutes (10 by default; 0 turns it off) to the
Backups folder in the settings folder (or one you choose) and keeps the newest few per
project. A backup never changes your project or its unsaved state.

## Undo history

{cmd:Undo} and {cmd:Redo} undo every edit, one gesture at a time. The history is kept on
disk and survives closing PlumSpirit: {cmd:UndoHistory} lists every step — click one to
jump back or forward — and shows its disk use with **Compact History** and **Clear
History…**. Settings outside the history (Onion Skin, the loop, Mute / Solo, the
SoundFont) are not undone.

## Scores (.psc)

{cmd:SaveScoreAs} saves the selected notes as a score file; {cmd:InsertScore} inserts one
at the edit cursor (you can also drop a `.psc` on the window). Scores keep each note's
subchannel, group and mute state.

## Change PPQN (Experimental)

In Project Properties: rescale the project to another PPQN (1 – 2,147,483,647).
**Preserve beats** multiplies every position by new ÷ old (tempos stay, so it plays the
same); **Preserve seconds** keeps every tick and changes the tempos instead. **Preview**
lists every rounding before anything changes; Apply is one Undo step, and going up and back
again (960 → 30,720 → 960) is exact.

See also [MIDI Import and Export](#midi-import-export).
