# Tempo, Time Signatures and Markers

Every project has exactly one **Conductor**. It holds the tempo map, the time signatures
and the markers. Tempo is global: it applies to the whole song and every track.

Click **Conductor** at the top of the track panel (or {cmd:SelectConductor}): the lane
under the Piano Roll becomes the BPM lane. The Conductor has no notes of its own; to see the
music while you edit the tempo, turn on Onion Skin ({cmd:ToggleOnion}): the tracks are drawn
in their own colors. The Conductor belongs to the whole song, so Previous, Next and All Other
Tracks all show every track there; Specific Tracks shows the tracks you chose.

## Tempo

* Click in the BPM lane to add a tempo point, drag to move it, double-click to type an exact
  BPM. Right-click a point for **Step** (holds until the next point) or **Linear** (ramps
  to it).
* {cmd:AddTempoChange} adds one at the edit cursor; the transport's Tempo field edits the
  first tempo.
* Tempos are exact decimals between 3.5762788 and 60,000,000 BPM. A MIDI file stores whole
  microseconds per quarter note; when Export has to round, it says how.

## Time signatures

{cmd:AddTimeSignature} or right-click the ruler → **Add Time Signature Here…**: beats per bar
and the beat note (1, 2, 4 … 128). Double-click its label on the ruler to edit,
right-click to delete. The grid, bar numbers and {cmd:PreviousMeasure} / {cmd:NextMeasure}
follow every change.

## Markers and chapters

{cmd:AddMarker}, or double-click the band above the ruler: a title, and **Point** or
**Chapter** (with an end). Click a marker to go there, drag it to move it, drag a chapter's
edges to change its range, double-click to edit.

## Master Volume and playing time

**Master Volume** (an Event Graph can show it) scales the whole mix and is exported as a
Universal SysEx Master Volume message. {cmd:PlayingTime} shows the song's length, the time
to the playhead, the selection and a loop, following every tempo step and ramp.

SMPTE-timed MIDI files have no tempo or bars; Format 2 files keep their tempo inside each
sequence.
