# Event Graphs and Curves

{cmd:ToggleVelocityLane} shows the graphs under the Piano Roll (drag the divider to resize;
**Graph 2** adds a second one). Each graph's header button picks what it shows — Velocity,
Pitch Bend, a named controller, **Generic CC** (all 128), Master Volume or BPM — and **Ch**
its MIDI channel. Both follow the Piano Roll's zoom. Click a graph to make it the
**active** one: it receives the tools, the selection, Delete, Copy / Paste and curves.

## Editing events

* Click empty space to add an event, drag to move it (a selected one moves the selection),
  Ctrl+click to select several, Shift+drag to box-select, double-click to type a value.
* Velocity bars belong to the notes: drag one to set the selected notes' velocity.
* **List** opens the {cmd:ToggleEventList} of the active graph.

On Windows, selected CC, Pitch Bend and Master Volume points can be pasted
into a matching graph in another PlumSpirit window. For CC and Pitch Bend,
choose the same MIDI channel in the destination graph.

## Curve tools

Drag from the start value to the end value: **Line**, **Gentle**, **Fast**, **S Curve**,
**Random** (seeded) and **f(t)** (a custom function); **Free-Hand** follows the pointer. The
Curve menu sets the interval (snap, note values, beats, measures or ticks), Replace or Merge,
**Only Before the Note**, the seed and **Anchors**. {cmd:CustomFunctions} edits named
formulas with a live plot (they use `t`, numbers, `pi`, `e`, `+ − * / ^` and functions such
as `sin`, `exp`, `clamp`; nothing else can run).

On the Velocity graph a curve sets each covered note's velocity; on BPM a Line becomes one
exact ramp. Every gesture is one Undo step.

## Graphs in tool windows

Many tools take a graph (a curve over the selection). In their curve editor every point can
be dragged, typed, added (double-click or **+**) or deleted; **Smoothness** blends straight
lines and the smoothest path, and each point can have its own **Smoothing** (Smooth / Not
smooth / Custom). **Outside** says what the graph gives before its first and after its last
point (Clamp, Extend, Repeat, Reflect). Graphs that map into a range can allow **Overshoot**
(0 – 200 %); the tool still keeps its results inside its limits.

{cmd:SaveAutomationAs} saves a graph selection as a score.
