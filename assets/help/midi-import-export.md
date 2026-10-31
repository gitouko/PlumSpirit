# MIDI Import and Export

## Opening a MIDI file

{cmd:Open} reads a Standard MIDI File (Format 0, 1 or 2). Every MIDI track becomes a
track — PlumSpirit never invents structure. A track whose notes use several MIDI channels
gets one subchannel per channel, so the channels stay visible. Everything that is not a
note (tempo, time and key signatures, controllers, program changes, pitch bend, SysEx,
text, markers, unknown events and chunks) is kept and written back on export.

A damaged file is refused with the place of the problem, or opened with warnings
(**Details** in the status bar) when its data can be kept safely.

## Importing into a track

{cmd:ImportIntoTrack} adds another MIDI file to the active track: **Source tracks as
subtracks** or **as subchannels**, inserted at the beginning or the edit cursor, with the
file's Conductor kept, merged, replaced or ignored. A Preview shows the result first;
Apply is one Undo step.

## Exporting

{cmd:Export} writes a `.mid`. The file is read back and checked before it replaces
anything. When MIDI cannot hold something exactly, the dialog says so and asks first:

* **Tempo ramps** become small tempo steps (every note within 0.5 ms of the exact ramp).
* **PPQN above 32,767** is rescaled for the file; your project keeps its own.
* **Chapters** become a marker at the start and one at the end.
* **Keys above 127** (256-Key Mode): Reject Standard Export (the default), Discard,
  Fold Down by Octaves, Clamp to Key 127, or an Experimental 256-key file.
* **Subchannels** are not MIDI: each note is written on its real MIDI channel; several
  subchannels on one channel can be kept apart in extra MIDI tracks. Colors are never
  written into MIDI data.
* **Track corrections** (Track Properties) are applied by default; the dialog can write the
  original notes instead.

**File → MIDI Export: Use Running Status Compression** (on by default) leaves out repeated
status bytes — the standard compact form some tools call "RSB compression". Off writes
every status byte, for players that mishandle running status. Both files hold the same
events.

**Program Change 0 for players that need a control event** (Settings → Import / Export, on
by default). When the file would hold no controller, program change or pitch bend at all, one
Program Change 0 — the General MIDI default instrument, so nothing sounds different — is
written at the start on the first channel the notes use. Kiva's Pre-render audio engine stops
responding on a file without one whose notes are all below its Audio Note Threshold. Notes,
velocities and timing do not change; the status bar says when it was added.

**Piano From Above** raises a quiet song: it scales the velocities so that the file's loudest
note plays at 127. A song whose notes are all velocity 1 plays at full volume there although
the file stores velocity 1; give one note a higher velocity to keep the others quiet. Details
of the export name the loudest velocity and the factor. Or turn on **Add a Silent Loudness
Reference for Piano From Above** (Settings → Import / Export, off by default): the file gets
one silent velocity-127 note on an unused, muted channel, and Piano From Above plays the
velocities as written (players count one extra note).

**Strict export.** A project opened from a MIDI file and not edited since (or with every
edit undone) is exported from the original file itself, event for event and in its order;
with Running Status Compression on, a standard compact file comes out byte for byte. The
original must still be where it was and unchanged; otherwise the project is written from
its contents and the status bar says why.

## Notes about MIDI

* Tracks are not channels: each note keeps its own MIDI channel and velocity.
* Note Offs are paired first in, first out per channel and key. Overlapping notes of one key
  can pair differently in other programs; after an export, Details says how many such notes
  the file holds.
* Middle C is C4 = key 60.

See also [Projects and Files](#projects-files), [MIDI Player and MIDI
Inspector](#player-inspector).
