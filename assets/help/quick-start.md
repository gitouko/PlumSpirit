# Quick Start

From nothing to an exported MIDI file. Each step names the menu entry and its current
shortcut.

1. **Create or open a project.** {cmd:NewProject} asks for a name, a tempo (BPM) and a
   PPQN (ticks per quarter note; 960 is a good start). {cmd:Open} opens a PlumSpirit
   project (`.lup`) or imports a MIDI file (`.mid`) — the MIDI file itself is only read,
   never changed.
2. **Look at the Track List.** Every project opens in the Track List: one row per
   track, its clips on the timeline. The ruler at the top shows bars; click it to move
   the edit cursor.
3. **Add a clip.** Right-click a track's empty timeline → **Add Piano Roll Clip Here**,
   or press {key:AddClip} to add one at the edit cursor on the active track. Drag a clip
   to move it, drag its right edge to make it longer. A track without a clip (a new
   track) is read-only in the Piano Roll, with a note saying so, until you add one.
4. **Open it in the Piano Roll.** Double-click the clip or press {key:OpenClip}. The
   corner of the Piano Roll says which clip you are editing. {key:SwitchView} goes back
   to the Track List at any time.
5. **Draw notes.** Choose the Draw tool ({key:Tool(Draw)}) and click an empty place for a
   note one Snap unit long, or drag to set its length. Snap (on the toolbar) sets the
   grid.
6. **Edit them.** With the Select tool ({key:Tool(Select)}) click or box-select notes,
   drag them to move, drag a note's right edge to resize, and use the handles of the
   selection's box to stretch it. Right-click a note for every tool. {cmd:Undo} and
   {cmd:Redo} undo and redo every edit, one gesture at a time.
7. **Play it.** {cmd:PlayPause} plays from the edit cursor; {cmd:Stop} stops. PlumSpirit
   plays through its built-in SoundFont synthesizer (choose one with
   {cmd:ChooseSoundFont}) or a MIDI output device (Settings → Playback & External MIDI → Audio Settings…).
8. **Save the project.** {cmd:Save} writes a `.lup` file that keeps everything exactly,
   including what MIDI cannot store (tempo ramps, folders, clips, subchannels).
9. **Export MIDI.** {cmd:Export} writes a standard `.mid` file. It is read back and
   checked before it replaces anything, and anything MIDI cannot hold exactly is shown
   for you to approve first.

{fig:draw-notes}

> You can turn off the quick-start card on the start screen with the Help menu entry
> {cmd:ToggleOrientation}. This page is always under **Help → Quick Start**.

Next: [Introduction](#introduction) for the window's parts, [Glossary](#glossary) for
the terms, [Keyboard Shortcuts](#shortcuts) for every key.
