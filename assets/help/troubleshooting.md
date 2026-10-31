# Troubleshooting

**No sound.** Check the output chip in the transport row: Internal plays through the
SoundFont synthesizer. Playback → **Audio Settings…** shows the device and its state; if no
audio device is available, everything else keeps working and Play tries again. A track
routed to a missing MIDI device falls back to the internal synthesizer with a notice. Keys
above 127 (256-Key Mode) are always silent.

**Notes are late or missing in very dense passages.** Switch Audio Settings → Render mode to
**Pre-Render**, raise **Voices**, or use **OmniMIDI KDMAPI** as the output. Late and dropped
notes are counted in Audio Settings → Diagnostics; they are never removed from the project.

**The view is slow.** Choose **Fast** Rendering Quality in Settings → Rendering /
Onion, try the other renderer there (**Renderer**), and close the MIDI Inspector while
editing millions of notes. See [Performance and Diagnostics](#performance).

**A MIDI file does not open.** PlumSpirit names the place of the problem. A file with keys
above 127 can be opened as a 256-Key Mode project when PlumSpirit offers it.

**Export changed something.** The export dialog lists every conversion before it writes
(tempo ramps, PPQN above 32,767, keys above 127, chapters); your project keeps its exact
values. See [MIDI Import and Export](#midi-import-export).

**A shortcut does nothing.** Some keys only work in the Piano Roll (note tools) or the Track
List (clip keys); {key:SwitchView} switches. Check {cmd:ShowKeybinds} for your current keys.

**Another program's hotkey changes something.** A PlumSpirit shortcut fires only with exactly
its own Alt: another program's Alt+Shift+… hotkey (OBS recording, Discord) does not reach a
shortcut without Alt. If one still collides, change it in {cmd:ShowKeybinds}.

**A plugin fails.** Open {cmd:ShowLuaPlugins} and read its **Log**: every error names its file
and line. A failing plugin never changes the project. **Copy for a Bug Report** copies the
log without your user name or folders.

**The interface is too small or too large.** Settings → Interface & Accessibility → **UI
Scale**, or {key:ResetUiScale} to go back to 100 %.

**Something else.** {cmd:SaveDiagnostics} shows a report you can attach to a bug report:
choose which parts it holds (system, project size, audio, settings, log), keep your user name,
folders and file names hidden (on by default), read exactly what it says, then **Save Report…**
or **Copy to Clipboard**. PlumSpirit sends nothing, and the report never holds your notes or
your project; to share a song, export it yourself and attach it only if you want to.
