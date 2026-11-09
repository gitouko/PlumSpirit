# Custom Graph Laboratory

Install this entire folder through Plugins → Lua Plugins → Install, or copy it into
PlumSpirit's plugin folder. Enable it and grant Project read, Edit notes and Plugin
panels. Open one of its three commands under Plugins → Custom Graph Laboratory.
Requires the API 0.2 development build; the legacy API 0.1 curve editor is independent.

The Lua source explicitly implements three original mathematical interfaces:

- Editable cubic Bézier: drag four orange control points, or edit Point X/Y.
- Polar rose: custom signed axes, petals/radius/phase; a primary drag sets phase.
- MIDI envelope: click to add a point, drag it, secondary-click to remove it (minimum
  two); choose linear or step interpolation. Negative values are deliberately supported.

X/Y centers and spans define independent mathematical domains. Wheel zooms; middle
drag pans. Coordinates remain in the original model and are remapped for each view.
The plugin retains eight render-revision mappings and rejects input whose mapping is
no longer available. The host clips canvas drawing; this sample clips each of its 256
sampled segments with the native geometry service. It does not render large point lists.

Choose Generate notes (2–512 notes on Track 1) or Map selected velocity (selected
notes on Track 1; demonstration cap 8,192). Preview shows up to 512 green note cells;
green intensity represents velocity. It changes no musical data. Apply MIDI uses the
captured model/seed and produces one native Undo step. Edits invalidate Preview. Gate,
velocity jitter, deterministic LCG seed and graph-to-MIDI interpretation are explicit
Lua algorithms. Musical colors still come from the Track/Subchannel model.

Save preset stores the versioned plain graph model and original view in plugin-private
presets. Reset then Load preset restores them. Saving is an explicit independent file
action and remains saved if the later musical job is canceled. Apply also stores the
last graph in plugin configuration. Close without applying succeeds without musical
changes; Cancel job, native Cancel plugin and window X cancel the whole pending job.

The source integration tests exercise all three registered commands in 128/256-key
projects, an independent Bézier reference, selected-note velocity mapping, deterministic
private Preview/replay/Apply/Undo/Redo, presets, native pointer edits/zoom and cancellation.
Actual Windows development-build testing separately verified all three panels, Bézier
and polar dragging, envelope point insertion, wheel zoom, preset reset/restore, 64-note
Preview/Apply, native Undo/Redo and both cancellation paths on a 1,000-note fixture.
Window-edge resize attempts did not change the observed panel: real Windows resize
acceptance remains open; narrowing/DPI is covered only by native headless tests.
Editor overlays, custom tool-parameter surfaces, CC/Pitch Bend examples and dense-project
interactive/RSS/audio acceptance remain required work. This sample is not full release
or arbitrary graph acceptance.
