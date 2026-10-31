# Generators

Generators make new notes without a selection: **Tools → Generator**, with five Simple ones
(Lines, Rectangle / Square, Circles, Arcs / Curves, Polygons) and five Advanced ones (Custom
Shape, Parametric Equation, Fractal / L-System, Harmonic / Spirograph, Procedural Growth /
Topology). Each opens a tool window with Preview, Apply as one Undo step, Cancel, Reset and
presets, like every tool ([Note Tools](#tools)).

## Where the notes go

* **Place** — at the edit cursor with a **Length**, inside the ruler's **time selection**,
  or inside the **selected notes' box** (their time span and pitch range).
* **Lowest / highest pitch** — the box's key range (in the project's keys).
* **Resolution** — the grid every generated note starts and ends on: the Piano Roll's Snap
  (the default), a note value or ticks.
* **Velocity**, **Gate** (how much of each grid step sounds), **Subchannel**.
* **Seed** — only on generators that draw random values.
* **Keep the selected notes** — off, the generated notes replace the selection.

The notes go into the active track, or into the clip you opened.

## Size and safety

A generator counts the notes it would make before it makes any, and the window shows the
estimate. A result above the limit is refused with the reason, never cut short. Every
generator has 25 advanced functions; hover over a control for what it does.

## Formulas

Parametric Equation and some functions take formulas (`x(t)`, `y(t)`, polar or cartesian).
Formulas run in PlumSpirit's own sandbox: numbers, variables and math functions only.
