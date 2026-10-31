# MIDI Player and MIDI Inspector

## MIDI Player

{cmd:ToggleMidiPlayer} shows, in place of the editor, the MIDI file the project exports to —
every track, corrections applied, clips placed, muted notes and clips left out — as notes
falling onto a keyboard (128 keys, or 256 in 256-Key Mode), in the style of Piano From
Above. It plays on the project's own transport (Space, seeking, loop and speed work as
usual); Escape or {key:ToggleMidiPlayer} returns to the editor.

* Notes fall at the same speed whatever the tempo; white-key columns are drawn first, black
  keys over them, and a later note covers an earlier one. Each track and subchannel has its
  own saturated color.
* The keyboard lights each key in the color of its latest-starting sounding note.
* The panel at the top left shows Time, FPS, NPS, Peak NPS, Polyphony, Peak Polyphony, BPM,
  Notes and Passed — exact right after any seek.
* {cmd:MidiPlayerSettings}: the panel's lines, **Falling time** (the song time between the
  top and the keyboard — any positive number of seconds, from a fraction of a millisecond to
  hours; 0 or a negative number is refused with a note in the field), and two looks of the Player only — **Note gradient** (each falling
  note shades darker to the right; about 40 % is Piano From Above's look) and **Hit glow**
  (a soft glow above each sounding key). The Piano Roll never uses them.
* **Notes fall by** Time (equal seconds are equal heights, one falling speed through every
  tempo change) or Ticks (equal beats are equal heights, so a note is as long as its beats
  and the speed follows the tempo; the window is the falling time at the song's opening
  tempo). Only the picture changes — what you hear, the keyboard and the panel stay the
  same. The top right says how far ahead the top is ("Top: 2 s ahead", "Top: 4 beats ahead").

Mute and Solo change only what you hear; the Player still shows every exported track.

## MIDI Inspector

{cmd:ToggleMidiInspector} opens a window that says what the exported MIDI file contains:
**Notes**, **Max NPS**, **Max polyphony**, **Duration** (through the tempo map), **Timing**
(PPQN), **BPM** at the start with the lowest and highest, and the number of **tempo
changes**. Every value is exact (marked **Exact**; right after an edit **Updating…** until
the analysis catches up).

Below are **NPS**, **Polyphony** and **Tempo** charts of the whole song. Each column shows
the highest value in its stretch of time, so no peak is ever hidden.

* **Ctrl+wheel** zooms in time at the pointer, the wheel or a right / middle drag scrolls,
  **Fit** shows the whole song; **Alt+wheel** zooms a chart's height (**Fit Y**);
  double-click fits both.
* **NPS Peak** / **Poly Peak** jump to where the highest value is first reached.
* **Axis** switches between song time and bars; **Log** draws the tempo on a logarithmic
  scale.
* Click or drag on a chart to move the playhead.
* **Follow Playhead** (on by default, saved independently of the editor) follows all
  three charts with page turns at the Playback menu's **Follow Position**. Manual
  scrolling, panning or zooming pauses following; **Following paused** and **Resume**
  appear above the charts. Resume, Play, Stop or seeking brings following back.
  Pausing playback keeps the view still. The whole-song view already contains the
  playhead. Page turns keep the zoom and each chart's height; peaks remain exact.

After you save, the Inspector checks its values against the saved project and says so.
