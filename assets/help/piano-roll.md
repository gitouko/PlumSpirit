# Editing Notes in the Piano Roll

The Piano Roll shows the notes of one track (or of the clip you opened): time runs left to
right, keys bottom to top, the keyboard on the left. The toolbar's Snap sets the grid that
drawing, moving and resizing follow.

{fig:draw-notes}

## The note tools

* **Select** ({key:Tool(Select)}) — click a note to select it; Ctrl+click toggles,
  Shift+click adds, drag on empty space for a box. Drag a selected note to move the
  selection; drag a note's right edge to resize.
* **Draw** ({key:Tool(Draw)}) — click an empty place for a note one Snap unit long, or drag
  for its length. With Note Audition on you hear it.
* **Erase** ({key:Tool(Erase)}) — click a note, or hold and sweep: everything you touch is
  removed when you release, as one Undo step. Other tracks and ghost notes are never
  touched.

Escape (or a right-click while drawing or erasing) cancels a drag. Right-click a note for
the note menu: every tool by kind, Mute / Unmute, Slice, Delete, Repeat Last Tool and the
recent tools.

## Gate, velocity, quantize

* **Gate** and **Vel** in the toolbar show the selected notes' length (ticks) and velocity;
  type a value and press Enter to set it on every selected note. The ▾ beside Gate chooses
  the length of new notes (Grid, Measure, 1/1 … 1/32 triplet).
* {cmd:Quantize} opens the Quantizer tool with grid, gate and Preview controls for the selected
  notes or the whole track. A Preview shows the result first.

## The selection's handles

{fig:scale-handles}

* Drag an **edge handle** to stretch the selection in time or pitch, a **corner** for both.
  Shift snaps; Alt on an edge fills the rows a stretch adds (or reverses **Tools → Simple → Scale
  Handles: Duplicate Notes** for one drag).
* **Alt + drag a corner** rotates the selection — about its center, or the opposite corner
  (Tools → Simple → Rotation Pivot). Shift snaps to 15° steps. The ruler shows the angle.
* **Ctrl + drag a corner** skews; **Ctrl + Shift + drag** moves only that corner
  (Perspective).

{fig:rotate}

{cmd:FlipX}, {cmd:FlipY}, {cmd:ScaleX} and {cmd:ScaleY} are in Tools → Simple. Scale Y
can transform the notes, fill the rows it adds, or copy notes by a pitch step. **Scale Y:
Prevent Overlaps** (on by default) cleans up the selected notes a shrink leaves on one key:
**Exact duplicates only** (default) keeps one of notes with the same start and length,
**Trim earlier overlapping gates** cuts a note at the next attack on its key, and **Remove
later overlapping notes** keeps the earliest. Other keys, tracks, channels and unselected
notes are never touched; the Preview shows the result, and one Undo restores every note.

## Copy, paste and more

* {cmd:Copy}, {cmd:Cut}, {cmd:Paste} (at the edit cursor). {key:Delete} deletes the selection.
* {cmd:SelectAll}, {cmd:SelectRange} (by bars and ticks, in one or many tracks).
* {cmd:SliceNotes} cuts the selected notes at the edit cursor (or at a time selection's
  edges).
* {cmd:DuplicateOctaveUp} / {cmd:DuplicateOctaveDown} copy the selection an octave away.
* {cmd:MuteNotes} / {cmd:UnmuteNotes} keep notes in the project but silent.
* {cmd:GroupNotes} / {cmd:UngroupNotes}: grouped notes are selected together.
* {cmd:RepeatLastTool} runs the last tool again on the selection;
  {cmd:RepeatLastToolNewSeed} with a new seed; {cmd:ReopenLastTool} opens its window.

## Note Overlap Prevention

**Edit → Note Overlap Prevention** (off by default) makes tools, handles, Scale, Flip,
Quantize, Paste and Draw remove or shorten the overlaps they would create — exact
duplicates, the same key and start, or same-key overlaps in time. Its submenu chooses what
counts and whether notes already there are included. Moves, Undo / Redo and imported files
are never changed by it.

## Time selection and loop

{fig:ruler-selection}

Click the ruler to move the edit cursor; **drag** along it to select a stretch of time;
**Shift+drag** sets the loop. Right-click the ruler for **Loop Selected Time**, **Select
Notes in Time Selection** and **Clear Time Selection**.

See also [Zoom, Onion Skin, Colors and Keys](#view), [Note Tools](#tools), [Event Graphs
and Curves](#graphs).
