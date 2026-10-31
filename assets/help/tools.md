# Note Tools

The **Tools** menu (and the note menu: right-click a note) holds every note tool, grouped by
kind. A tool works on the selected notes of the track or clip you edit.

## The tool window

Tools → Simple contains {cmd:FlipX} and {cmd:FlipY}, which apply directly, and
{cmd:ScaleX} / {cmd:ScaleY}, which keep their factor entry and Preview. Advanced
Flip retains its full window, functions and presets. {cmd:Quantize} opens the
Quantizer tool; the old shortcut still works.

Other tools open a small window beside the Piano Roll:

* **Preview** (on by default) draws the result in the Piano Roll while you change the
  controls; you can still scroll and zoom. Nothing changes until **Apply**, which makes
  exactly the previewed change as one Undo step. **Cancel** changes nothing; **Reset** brings
  the defaults back.
* **Presets**: Save, Save As, Load, Rename, Delete. A preset keeps every control.
* **Seeds**: a tool that draws random values shows a seed, so the preview is exactly what
  Apply writes and a preset gives the same result again.
* Each tool has its own controls, then **advanced functions** (each switched on by its box),
  then four shared ones: **Subchannel Offset Driver**, **Pitch Offset Driver**, **Remove Note
  Overlaps** and **Control Sweep** (a control follows a graph across the selection). With
  several functions on, the line under Preview gives the order they apply in.
* Hover over any control for what it does. Impossible expansions are refused before
  anything is built; a result too large for memory says so instead.

{cmd:RepeatLastTool} repeats the last applied tool on a new selection, {cmd:RepeatLastToolNewSeed}
with a new seed, {cmd:ReopenLastTool} opens its window again.

## Sections

* **Simple** — quick Flip X/Y, Scale X/Y and the scale-handle settings.
* **Piano Roll Tools** — FL-style tools: Arpeggiator, Articulate, Chopper, Chord
  Progression, Chord Stamp, Claw Machine, Flam, Limit, Quantizer, Randomizer, Riff Machine,
  Strum and more.
* **Note Editors** — note properties, velocities, gates, transposition, repeats, groups.
* **Brush & Selection Tools** — brushes and selection tools you use with the pointer.
* **Geometry** — shapes in time × pitch: scale, rotate, skew, perspective, ripple, tiny
  planet, displacement maps, outlines, trails, warp.
* **Iterations** — Chopper V2 (formerly **Note Repeat** / **Notes Repeat**), Paint V2
  (subchannel painting) and Duplicate V2 (copies with changes per copy). Chopper V2
  divides each original note into repeats; the original Chopper stays in Piano Roll Tools.
* **Experimental** — tools that may still change between versions (3D, Tumours …).
* **Generator** — tools that make new notes from nothing: see [Generators](#generators).
* **Custom** — tools from your Lua plugins: see [Lua Plugins and Custom
  Tools](#lua-plugins).
* **Legacy** sections keep replaced tools so old presets still load.

Every tool PlumSpirit has, with what it does:

{tools}


Numeric fields accept decimal and exponent notation and show the full accepted value when editing. Use a dot for decimals. Floating fields keep their native 32-bit or 64-bit representation; extra digits round to that representation. Integer fields reject fractional values. The range in a tool's specification is its semantic domain. Sliders have a checked numeric field beside them for precise entry. Invalid input shows an error and keeps the previous value.

Time and pitch calculations ultimately produce whole MIDI ticks and keys. Tools round their output to those discrete units; Preview and Apply use the same calculation. The Scale dialog uses an exact decimal factor before tick rounding. Tempo and playback speed use exact decimal storage within the supported timing representation. Presets preserve accepted fractional parameters and graph point coordinates when saved and reopened.

## Warp Transform tiles

Wrap X and Wrap Y keep No, Tile and Reflect, and also offer **Y-Flip Tile** and
**180°-Tile**. On alternating tiles along the chosen wrap direction, Y-Flip Tile
mirrors pitch; 180°-Tile reverses both time and pitch. Negative tiles alternate
in the same way. Using both directions composes their flips. Preview shows the
same result that Apply commits, including the clipping at the selection box.
