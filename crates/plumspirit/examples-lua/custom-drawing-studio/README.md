# Custom Drawing Studio

Install the whole folder using Plugins → Lua Plugins → Install. Enable it and grant
Project read, Edit notes and Piano Roll drawing tools (tools.draw). Open a project,
then choose one of the five tools under Tools → Custom → Custom Drawing Studio.
This example requires the API0.2 development build.

Draw inside the Piano Roll with the primary pointer, release, inspect the temporary
Preview, then Apply. Each Apply is one native Undo step, and the tool stays open for
the next gesture. Undo/Redo use the application's ordinary history. Escape cancels an
active stroke; Clear stroke or Cancel drops the unapplied result. Starting a new stroke
replaces the previous unapplied stroke. No notes change during capture or Preview.

All five artistic algorithms are explicit Lua in main.lua:

- Freehand Brush rasterizes the captured path and paints an authored vertical footprint.
- Rectangle Shape interprets first/last samples as a filled or outlined rectangle, using
  the shared native polygon/polyline note-cell rasterizer.
- Deterministic Spray uses an explicitly written32-bit LCG, rectangular scatter and
  velocity falloff. The captured stroke plus Seed produces the same result on replay.
- Note-Onset Eraser uses an indexed native start-range query, then the general native
  nearest-polyline predicate to delete onsets inside the authored brush radius. It
  intentionally tests note onsets, not every pixel of a long note body.
- Mathematical Wave resamples the captured path and explicitly deforms it with a sine
  function before rasterization. Cycles and Size control this authored equation.

Brush parameters use the existing native custom-tool form. Presets save/restore those
parameters and seed. A captured stroke is temporary input and is not stored in a brush
preset. Step controls horizontal musical resolution; Gate and Velocity control notes.
Subchannel ID must already exist on the edited Track. Native Track/Subchannel colors
remain authoritative; this example does not invent per-note RGBA storage.

The example reads the immutable native stroke in1024sample batches, retains an authored
LOD path of at most2048points (512for spray/eraser/wave), limits each gesture to4096grid
columns and8192generated notes, batches writer output256notes, and caps eraser candidates
at8192before Lua predicates. Host cancellation, memory, transaction ownership/locks and
destructive confirmation rules still apply. A rejected operation leaves no partial notes.
A native capture at its65,536sample limit is refused so a truncated stroke is not applied.

The host retains bounded ordered native frame-event batches, with host handling times
rather than individual device timestamps; pressure/tilt and per-sample modifier
history are not supplied by this initial adapter. Rich custom overlays/live brush events,
image/font resource composition, additional gestures and dense/RSS/audio acceptance remain
required API work. Existing native Draw/Paint and API0.1 curve tools remain available.
See docs/v1_0_1_api02 evidence for source/headless and actual Windows test distinctions.
