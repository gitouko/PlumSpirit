# PlumSpirit changelog

## v1.0-Launch (1.0.0) - 2026-10-29

An engineering milestone, not the public release. Measured on Linux in the cloud and checked
on the owner's Windows PC (KDMAPI switching and listening, Kiva, Piano From Above, songs up to
562 million notes); the checks still open are listed in docs\KNOWN_ISSUES.md.

- Switching the output to OmniMIDI (KDMAPI) while playing no longer crashes (route-count
  panic); outputs open on their own threads, join at the playhead and never hold Play,
  Pause or the playhead.
- External MIDI is sent at each message's own time (the rapid "spam" tone keeps its
  rhythm); an overloaded output waits or drops counted notes, never silently.
- Project-created MIDI exports re-checked byte by byte with an SMF analyzer; the Kiva hang
  found with it is repaired (below) and checked in the owner's Kiva.
- Opens MIDI files from zip, 7z, gz, xz, zst and tar archives (nested too), and damaged
  files in Compatibility Mode with every repair listed. RAR is reported, not opened.
- Selection Analytics charts (NPS, polyphony, note count, tempo) with playhead, hover,
  Follow and zoom; a compact readout with details on Alt.
- One Settings window in twelve groups, a decluttered View menu, check boxes that keep
  menus open, persistent Onion and Y-zoom defaults, Fit 128 / 256 Keys, trackpad Y zoom
  in the Track List and the Piano Roll, Smooth Follow at 100 %.
- Play Only Selected Tracks (Alt+P) and Restore All Tracks (Alt+A) without touching Mute /
  Solo; large current-track M / S. Clips selected on several tracks play their tracks.
- Onion Skin at scale: background overview, no blank or wrong frames, per-track Replace /
  Add / Remove; keyboard colours follow the roll; another program's Alt+Shift hotkeys no
  longer reach PlumSpirit shortcuts; no bounding box around grouped notes.
- Import uses every MIDI worker on big tracks (up to 1.8x faster on two cores) and shows its
  real stage; loading can be canceled from the status bar.
- Closing a busy project stops its workers at once (a 60M-note close 2-3x faster); a memory
  guard stops opening a file before the PC runs out of memory, and a project that fits
  with little room left opens in low-memory mode.
- Windows build (2026-10-30, measured on the owner's PC with a 224-million-note, 700-track
  community MIDI): a faster memory allocator (mimalloc) — closing that project takes 0.5 s
  instead of 3 s without the 130 ms freeze, the note analysis 1.5 s instead of 8.4 s, import
  1.4 s instead of 1.7 s; switching the Onion to All Other Tracks while paused or playing no
  longer stalls one frame for ~0.35 s (the keyboard colours read their key lists a piece per
  frame).
- Windows, 448 million notes (2026-10-30, the same community MIDI duplicated): Undo of a
  linked duplicate of every clip no longer freezes the window for 7 s (the note analysis
  is rebuilt in the background), and the analysis of tracks that play through clips uses
  every analysis worker; the Import Into Current Track preview of a 224-million-note file
  no longer takes 12.7 s a frame (it stops reading once its 20,000 drawn notes are shown),
  so Apply responds at once. Selecting or undoing clips over hundreds of millions of notes no
  longer marks every note on the UI thread for the Selection readout (0.56 s on Windows);
  a MIDI output that cannot keep up is reported at most once a second (the log got a line
  a frame at 1.8 million notes / s).
- Windows, 562 million notes (2026-10-30, Toilet Story 3 + Introverted + Toilet Story 3 by
  Import Into Current Track): the Apply of a 224-million-note Import Into no longer holds
  the window 5.4 s (its undo history record is written on a worker; the imported file's
  copy is freed in the background) and its Undo no longer 12.2 s (the analysis is rebuilt
  in the background on every analysis worker: 1.2 s instead of a 12.8 s single-thread
  patch). Discarded redo steps, replaced analyses and Onion tiles of notes that are gone
  are freed off the UI thread. F12 names the slowest paint and the slowest update of the
  last 120 frames with their largest parts.
- Onion Skin on many tracks (2026-10-30): changing the ghost opacity, outlines or a colour,
  or editing one track, no longer copies every other track's notes for the Onion pictures
  (1,430 tracks: 0.65 ms a step instead of 8.3 ms in the development test; 35-59 ms on the owner's PC at 562
  million notes before). F12's slowest frame is split into PlumSpirit's update, egui and
  the graphics layer (OpenGL: picture uploads vs drawing), and frames spent in a file
  dialog no longer count as slow frames.
- Kiva (2026-10-30, after the owner's run): a MIDI file exported from a project that has no
  controller, program change or pitch bend gets one Program Change 0 (the General MIDI default)
  at its start, so Kiva v1.1.18-1m's Pre-render audio engine no longer hangs on velocity-1 art
  (Settings → Import & Export, on by default; notes, velocities and timing unchanged). The
  export's Details explain Piano From Above's volume correction, which plays a song whose loudest
  note is velocity 1 at velocity 127 although the file holds velocity 1. New opt-in export option
  (off by default): a silent loudness reference — one zero-length velocity-127 note on an unused,
  muted channel — so Piano From Above plays the velocities as written.
- Help: the exact Introduction text, no fixed 2 GB promise. One README in the release
  folder; changelog, known issues, build information and legal notices in docs\ and legal\.
- Projects, MIDI files, presets and Lua plugins of earlier versions keep working. Source is
  provided without an open-source license, as requested by the owner.

## v1.0-beta2 (1.0.0-beta.2) - 2026-10-27

- Preserve stable Track colours and layering; correct Conductor Onion colours and
  consistent thin dense-note outlines. Help paragraphs and narrow-window layouts fit.
- Explicit no-Clip lockout, individual Clip selection and virtual all-Clips editing;
  cross-Track tools retain source ownership and checked song/local timing.
- Configurable Advanced Function order within supported phases, precise numeric
  entry, bulk Add Tracks with hierarchy and Undo, Accent Color, sharp corners,
  separate Note/Track preset import/export and cyclical Rainbow-16 modes.
- Player Time/Tick visualization and unrestricted finite positive typed Falling
  Time; opt-in playback-only key/velocity/reset/length/Note-Off processing.
- Profiled synth shard balancing, render lanes and event batching; persisted voice
  policies with deterministic seek/loop behavior. MIDI export remains unchanged.
- Simple Flip/Scale routes, Advanced Flip label, Chopper V2 in Iterations, Warp Tile
  Y-Flip/180-degree options, Scale Y overlap cleanup and no duplicate Edit Quantize.
- Spiderweb shared playback cursor, Track List clipboard/select/nudge/framing keys,
  independent Inspector Follow Playhead with Resume after manual navigation.
- Close Project releases project caches, textures and worker/audio owners off the
  UI thread; completion and process-memory Details explain global/allocator retention.
  Two close cycles each at 1M/10M/40M verified without working-set trimming.
- Existing project, MIDI, preset and Lua plugin compatibility retained. Source is
  provided without an open-source license, as requested by the owner. Physical
  listening/hardware acceptance remains explicitly listed in beta2 records.

## v1.0-beta1 (1.0.0-beta.1) - 2026-10-24

Invite-only community playtest beta; **not v1.0 final**. Everything from v0.9 still works;
v0.9 projects open unchanged. Requirements, evidence and what still needs a Windows
playtest: `docs/v1_0_beta1/` (SESSION_CAPSULE.md, REQ_GRAPH.toml, KNOWN_LIMITATIONS.md,
UI_ACCEPTANCE.md).

### Fixed (reports from the v0.9 playtest)

* A clip moved to another track or subtrack no longer shows 0 notes; a merged clip shows
  the merged notes everywhere (also in subtracks).
* Selection Opacity is applied by every tool's preview.
* Pause / resume no longer strikes a held note again (Audio Settings can still ask for it).
* **Strict no-edit MIDI export**: an unchanged import is written from the source file
  itself — every track, event and same-tick order kept (the duplicate tempo of
  tau2.5.9 at tick 480, the 160 RDR tempo on track 1); **Running Status Compression**
  on / off; certified on the owner's files with a streaming comparator.
* Piano Roll: never-blank dense regions while zooming / scrolling, no flicker on resize,
  flat fills with dark outlines (no gradients or shadows), stable colors and
  track-order layering, one color per keyboard key.
* Closing a huge project returns at once (projects are freed in the background).

### New

* **Lua plugins** (Lua 5.4, sandboxed, versioned API `plum`): projects, tracks / subtracks
  / folders, clips, notes in native batches, controllers, selection, timing and analysis;
  every edit Preview = Apply and one Undo step; budgets, Cancel, threads; plugin packages
  with install / update / remove, a log with redaction, settings and presets;
  **Tools → Custom** note tools with native forms; two example plugins;
  `docs/LUA_API.md`.
* **Help** (F1): a searchable guide for every feature since v0.1, Quick Start and
  first-launch orientation.
* **Settings** in one window (search, reset), **keybind editor** with export / import,
  **UI scale 30 – 175 %**, **Tab** switches Track List ↔ Piano Roll, consistent number
  boxes with − / + steppers, tooltips everywhere, American English, a calmer design with
  progressive disclosure, the **official PlumSpirit logo**.
* **MIDI output**: Internal SoundFont, Windows MIDI devices and **OmniMIDI (KDMAPI)**,
  with a safe fallback to the internal synthesizer; **two playheads** (edit / anchor and
  playback).
* **MIDI Player**: Piano From Above–style rainfall (top order, sub-pixel culling, one
  lit color per key), its own optional effects, timebase and statistics.
* **Editing**: Note Properties **Math expressions** (Experimental) and **Ranges** with
  graphs; per-point graph **smoothness** and **overshoot**; **Alt + corner free rotation**;
  **Scale Y Duplicate Notes** and Scale Y overlap prevention; **General Note Overlap
  Prevention** (off by default); **Change PPQN** (Experimental); PPQN 1 – 32,767 and
  BPM up to 60,000,000 checked honestly; **Fit 128 / 256 Keys**, Lock Fit Range and an
  optional Y zoom beyond the keys; Paste Repeat (notes) is now **Duplicate V2** under
  Iterations.
* **Track List**: Slice (Ctrl+K), Copy / Cut / Paste clips; square clips with an opaque
  header and a transparent body; the Progressor and Selection Analytics over the clips.
* **Analytics**: Selection Analytics **gate range**; the Progressor follows Track
  Mute / Solo (on by default); the MIDI Inspector zooms / scrolls its NPS / Poly charts,
  jumps to peaks and shows a **Tempo** chart.
* **Help → Save Diagnostics Report…** opens a preview: choose the sections, keep your
  user name / folders / file names hidden, then save or copy exactly what is shown.

### Faster (2 vCPU test machine, `docs/PERFORMANCE.md`)

* MIDI import on worker threads (160 RDR 2.61 → 1.57 s), export and its validation on
  workers (160 RDR 9.0 → 3.6 s; strict 3.9 → 2.1 s), `.lup` save / load on workers
  (3.0 → 1.7 s / 1.68 → 1.02 s), bulk edits at millions of notes (10M paste 1.06 → 0.80 s),
  a benchmark ladder at 1M / 10M / 100M / 250M notes.

### Not verified yet

Physical audio / KDMAPI listening, the Piano Roll manual acceptance and the 18 owner
journeys need the owner's Windows PC (`docs/v1_0_beta1/UI_ACCEPTANCE.md`); they are
labelled NEEDS_OWNER_PLAYTEST, not passed.


## v0.9 ~ Prototype (0.9.0) - 2026-10-20

Owner-directed Windows playtest and source snapshot handoff for v1.0 feedback.
Development is wrapped up at the current implementation checkpoint. Physical
listening acceptance and the remaining integration/release gates are deferred,
not declared passed. No additional test suite was run for this packaging handoff.

* Clip-based Track List, clip operations and .lup persistence, MIDI Player and
  Inspector, shared export analysis, 256-key editing and explicit MIDI boundaries.
* Revised graphs, shared tool services, expanded tools and ten generators with
  25 advanced functions each. The approved S71 Experimental removals are retained.
* Dense visualization/Onion improvements and bounded synth shard parallelism;
  prior targeted verification and A/B performance evidence remain in docs/v0_9.
* Windows app, resource metadata and source package now identify version 0.9.0.
* Source supplied as a snapshot; licensing remains undecided by the owner.


## v0.8 ~ Prototype (0.8.0) — 2026-10-05

v0.8 brings **five built-in plugins**, an **undo history that survives
restarts**, a **rebuilt note visualizer** that never goes blank or flickers, a
**stronger synthesizer** with audio settings, a choice of **OpenGL or Vulkan**,
and the owner's list of editing improvements and fixes. Everything from v0.7
still works the same way; v0.7 projects open unchanged (saving adds the v0.8
sections — folders, reference audio — to `.lup` version 5, which v0.7 reads
without them).

### New

* **Built-in plugins** (Plugins menu; credits in Help → About and each window):
  * **Spiderweb** (Alt+W) — the shape engine of Kanade Tachibana's Spiderweb
    (MIT) ported to Rust and checked against the original program (1,081 shape
    cases, 2,824 formulas equal); expanded for PlumSpirit by ouko: free
    rotation, Scale X / Y with negative values, Flip X / Y, text in installed
    fonts with bold / italic / underline / strikeout / capitals and colours per
    line or letter, fixed shape colours, Take the selected notes, share codes,
    Listen, direct paste into the Piano Roll.
  * **Image to MIDI** (Alt+I) — after arduano's ImageToMidi: palette (median
    cut, Clusterise, your own colours), transparency, ignored colours, runs with
    a maximum length or one note per pixel, rotate / flip / scale, both
    orientations; pictures are decoded with size limits.
  * **HZ-Chop** (Alt+H) — Softy107's Andromeda Lua script rewritten natively,
    every note equal to the original (Original Lua Compatibility) plus a High
    Precision mode and Replace.
  * **SAFOR — Simple And Fast Overlaps Remover** (Alt+O) — after DixelU's SAFOR
    (Apache-2.0): overlaps, sustains and overlaps, velocity threshold, with the
    original's priority rules, in O(N log N), keeping Tracks, Subtracks,
    Subchannels and groups; scopes from the selection to the whole project.
  * **Audio Reference** (Alt+R) — a WAV / FLAC / Ogg / MP3 recording played in
    sync with the song (tempo map, seek, loop, speed — 1,000 loops without
    drift), volume, mute, offset, align, waveform overview; saved in the
    project, Relocate when missing.
  * Spiderweb and Image to MIDI paste colours into **new Subtracks** or
    **Subchannels of one Track** (or existing ones), with a **signed
    Subchannel Paste Offset** and a mapping preview; one undo step.
* **Persistent Undo / Redo**: a journal per project in the settings folder
  (checksummed, repaired when damaged, compacted on request); Edit → **Undo
  History…** (Ctrl+H) jumps to any state, also after a restart; disk use,
  Compact, Clear.
* **Open File History**: File → Recent Files and the start screen (pins, Locate
  for moved files, Remove, Clear Missing, Clear).
* **Track Folders**: create, rename, collapse, drag in / out, delete keeping or
  removing the tracks; one undo step each; saved in the project, never exported.
* **Time selection on the ruler** (drag) with **Loop Selected Time** and Select
  Notes in Time Selection.
* **Visualizer**: colour-aware dense tiles built in the background, never blank
  (stand-ins while building), stable Onion **Dense / Sparse First** order,
  outlines on every ghost with **Outline Darkness** 0–100 %, 20 hand-picked
  **Track colours**, keyboard colours for every track and subchannel, playhead
  triangle and line on one pixel.
* **Renderer**: View → Renderer → OpenGL (default) or Vulkan; GPU Automatic /
  Integrated / Dedicated; automatic fallback to OpenGL, no crash loop;
  `--bench-render`.
* **Audio**: Playback → **Audio Settings…** — device, sample rate, buffer,
  Realtime / **Pre-Render**, Maximum Voices 64–4,096, audition reserve, master
  gain with a safety limiter and meters, SoundFont list and effects,
  diagnostics. Dense bursts are deferred, not dropped; anything dropped or
  clipped is counted and reported.
* **Editing**: right-click note menu with every tool, **Repeat Last Tool** (F5,
  Ctrl+F5 new seed, Shift+F5 reopen), **Mute Notes / Unmute Notes** (Ctrl+M /
  Ctrl+Shift+M), **Ctrl+X = Delete**, Y-Zoom − / + and Ctrl+Shift+wheel, toolbar
  icons, semantic editors for every list parameter (graphs, step patterns,
  swatches, points, four-corner quad, lattices) and a rebuilt curve editor,
  **Paint** revamped (subchannel offsets by time / pitch / notes + 14
  functions; the v0.7 brush is now Note Brush), Note Properties **Offset
  subchannels**, Note Repeat **Pitch offset per repeat**.
* **Selection Analytics text** customizable like the Progressor (View →
  Selection Analytics Settings…).
* Help → About lists every built-in plugin with its author, original and
  licence.

### Fixed

* **Onion Skin / visualizer blackout**: dense views no longer go blank (a
  single viewport-wide texture exceeded the GPU's maximum size).
* **Onion layer order** no longer flips between frames or while paused
  (density snapshots with hysteresis, Track id tie-break); ghost outlines no
  longer appear and disappear (a 12,000-outline cut-off was removed).
* **Keyboard colours** show every track and subtrack, not one colour per key.
* **Synthesizer**: dense passages lost up to 75–98 % of their note-ons and
  clipped; they are now deferred within 50 ms and limited, and voices go beyond
  RustySynth's 256.
* **Progressor** follows every change while paused (paste, delete, undo / redo,
  plugins, tracks, tempo); one canonical definition of every statistic, shared
  with Selection Analytics.
* **Scale** no longer duplicates notes (a remembered Fill setting); Fill only
  with Alt or the dialog's box.
* **Join** merges only one note chain (same track, pitch, subchannel, channel,
  group, mute).
* **Graph adjustments** in tool windows work (select, drag, type, add, delete).
* **Playhead** top triangle aligned with its line at every DPI and zoom.
* Track commands follow the multi-selection and folders (Delete Track, Add
  Track Below, Mute / Solo); notes can no longer be pasted into the Conductor.
* Repeat Last Tool, one-click commands and Compact History no longer freeze the
  window on big selections (background jobs); history write failures are
  reported instead of ignored.
* An unset audio sample rate / buffer was logged as invalid at every start.
* Found in the release journey: a plain click on a selected note that was off
  the Snap grid (after Humanize, Perspective …) moved the whole selection onto
  the grid (and a click on a note's edge resized it); moves and resizes now
  start only when the pointer really drags.
* Undo / Redo of an edit on several tracks (SAFOR on the project, a tool on
  selected tracks) no longer jumps to another track: the active track stays
  when it was changed.
* Points on the border of a curve or point editor (curve ends, values at their
  limits) can be grabbed at their centre, not only on their inner half.
* Menus taller than the window scroll: on a 1366 × 768 screen View → Renderer
  and CPU / RAM in Status Bar were below the screen edge.
* "Save changes?" says what Don't Save keeps: the file stays as saved, the edits
  stay in the Undo History and can be redone after reopening (settings outside
  the history are lost).

### Performance

Measured in docs/v0_8/PERFORMANCE_BASELINE.md ("v0.7 → v0.8 comparison") and
docs/PERFORMANCE.md ("v0.8"): engine measurements on 1M notes unchanged within
±5 % (MIDI import −12 %); whole-song static frames 87–97 % faster (tiles);
Onion with 2,000 tracks: slowest frame 6.0 → 2.2 ms; SAFOR on 1M notes 0.3–0.5
s; idle RAM +3–4 MB; the binary is larger (image decoders, glyph outlines,
Vulkan). PCs with ≤ 2 logical processors start with 256 voices, PCs with ≤ 3 GB
RAM with a 24 MB tile cache.

## v0.7 ~ Prototype (0.7.0) — 2026-10-02

v0.7 turns the Piano Roll into a **Black MIDI transformation environment**: 64
note tools on one Note Transformation Platform, each with a live preview, one
undo step, presets and ten advanced functions; reusable `.psc` score files;
selection analytics; track management and many fixes. Everything from v0.6 still
works the same way; v0.6 projects open unchanged (saving writes `.lup` version 5).

### New

* **Note Transformation Platform** — every tool: movable modeless window, live
  Preview (on by default; the Piano Roll can be panned and zoomed meanwhile),
  Apply = one undo step, Reset, Cancel, presets (Save / Save As / Load / Rename /
  Delete), ten advanced functions, deterministic seeds, boundary policies (pitch,
  ticks, lengths), resource preflight that refuses impossible expansions before
  allocating, subchannels preserved by default.
* **Tools** (Tools menu): Arpeggiator, Articulate, Chopper, Chord Progression,
  Chord Stamp, Claw Machine, Flam, Limit, LFO, Quantizer, Randomizer, Riff
  Machine, Level Scaling, Strum; Note Properties, Humanize, Transpose, Legato,
  Join, Group, Note Repeat, Mute, Slide; Draw, Paint, Select (drawn lassos),
  Delete, Interpolate, Slice, Snap to Scale, Flip, Duplicate, Score Stretch;
  Skew, Distort, Perspective, Warp, Shear, Twirl, Wave, Clipping Mask, Paint
  Color; 20 experimental tools (Gravity Well … Recursive Tile). Zoom tool and
  Play Selected / Scrub.
* **Note groups** (nested, named, coloured, locked, temporary, linked; kept by
  copy / paste, undo and `.lup`) and **muted notes** (never played, exported or
  counted).
* **`.psc` score files**: Save Score As, Insert Score (preview, anchor,
  subchannel mapping), drag and drop, Save Automation As.
* **Selection scale handles** with Shift snapping, **Custom Snap**, **Scale Y
  Duplicate / Fill**.
* **Selection Analytics** (F6): count, NPS, PPS, poly, Peak Poly, Unique Pitch
  Poly, Poly Area.
* **Tracks**: add / delete anywhere, multi-select, drag-and-drop reorder and
  reparenting, Alt+arrows; **Close Project**; **Onion Skin outlines**;
  **Keyboard Shortcuts** window (F1) from one registry.

### Fixed

* The Subchannels window's **+** did nothing (it edited a copy of the document).
* New subchannels get the next MIDI channel (1–16 cycling, optionally skipping
  channel 10) instead of the track's default.
* Scale / Quantize / Import Into Track / Track Properties previews no longer
  block the Piano Roll (movable windows, no backdrop).
* Tools no longer misbehave with notes at the very end of the tick range (16
  tools could overflow there); notes that would end past it are dropped and
  counted, never wrapped.
* With several tracks selected (click, then Ctrl+click), Alt+↑ / ↓ / → and
  Delete act on all of them, also when started from the current track.
* Apply on a result identical to the selection (for example Paint without a
  new stroke) says "Nothing to apply" instead of adding an empty undo step and
  marking the project changed.
* Renaming a preset to the name of another preset is refused instead of
  replacing it.
* Ctrl+Plus / Ctrl+Minus no longer scale the whole interface by accident (an
  unlisted, unsaved built-in shortcut; Ctrl+0 is Reset Zoom).
* Contour Trace and other grid tools are faster on long selections (rings
  grow from a frontier instead of rescanning the grid).

### Performance

Measured in docs/PERFORMANCE.md ("v0.7"): a 1:1 tool on 1M notes applies in
≈ 60–90 ms with 15 bytes of undo per note; previews of millions of results are
aggregated and cached; v0.6 measurements are unchanged within noise except bulk
record edits (+9–21 %, the per-note group id).

## v0.6 ~ Prototype (0.6.0) — 2026-10-01

v0.6 adds **live insight into the music**: a Progressor readout with exact
note, NPS, polyphony and PPS statistics at any position of a multi-million-note
project, piano keys that light up in the colours of the notes sounding on them,
and note audition while composing. Everything from v0.5 still works the same way;
projects and settings from earlier versions open unchanged.

### New

**Progressor (View → Progressor, on by default)**

* A small readout in a corner of the piano roll: **Notes** passed / total,
  **NPS** (notes starting in the last second of song time), **Poly** (notes
  sounding now — stacked notes on one key count separately) and **PPS** (the
  average poly over the last second).
* Exact at **any** position the moment a project is open: click anywhere on the
  ruler and the numbers are right — no need to play up to it. Paused, looping,
  seeking backwards: always the same numbers for the same position.
* Tempo changes and BPM ramps are taken into account exactly; Playback Speed is
  an audition setting and does not change NPS.
* Follows every edit (insert, delete, move, quantize, paste, undo …); after very
  large edits the old numbers are shown dimmed with "Updating analysis…" for a
  moment, never a mix.
* **Progressor Settings…**: tick the fields (also Peak NPS, Peak Poly, Peak Poly in
  the last second, Progress %, Time, Bar, BPM), or write **your own text** —
  `{nps}`, `{poly}`, … are replaced by values, everything else is shown as typed;
  position (any corner + offset), font size, alignment, line spacing, text colour
  and opacity, background, number format (40,000,000 / 40000000 / 40.0M), PPS
  decimals, what counts (all project notes, audible tracks only, or the active
  track and its subtracks) and the NPS / PPS window. OK / Cancel / Reset to
  Default; kept in the application settings (never marks a project as changed).

**Live Keyboard Colors (View, on by default)**

* While playing or paused, every piano key shows the colours of the notes sounding
  on it — from every track, subtrack and subchannel (or only audible tracks, or the
  active track: View → Keyboard Colors Show). Busy keys show the three most common
  colours plus a blend of the rest; hover a key for the number of notes.

**Note Audition (View → Note Audition, on by default)**

* Press a piano key to hear it, drag across the keys to play each one, release to
  stop. Placing a note with the Draw tool plays it briefly (View → Placed Note
  Sound Length: 100–800 ms).
* Uses the active subchannel's MIDI channel and the track's instrument at that
  position, through the SoundFont — also while the song plays, without ever
  cutting a note of the song, and for muted tracks too. Stops when the window
  loses focus or a dialog opens. The SoundFont loads on the first click if needed.

### Improved

* The developer overlay (F12) shows the analysis memory and the audio device's
  callback size.
* Playback keeps at least two device callbacks of audio buffered (devices that ask
  for large buffers no longer starve).

### Performance

* The analysis costs about **3 bytes per note** for a dense project (under 1 byte
  for chord stacks; nothing while the Progressor and keyboard colours are off) and
  answers any position in **2–3 µs** at 1M and 5M notes, with 1 or 1,000 tracks.
  Typical edits update it in about a millisecond. See docs/PERFORMANCE.md.
* No measurable change to existing operations (A/B against v0.5).

### Known limitations

* Note audition always uses the internal synthesizer, also for tracks routed to an
  external MIDI output.
* Peak fields are computed in the background the first time they are shown.

## v0.5 ~ Prototype (0.5.0) — 2026-10-01

v0.5 starts the deeper development arc: tracks can now hold **subchannels** and
**subtracks**, MIDI import keeps a file's real structure, and every batch edit is
**previewed before it changes anything**. Everything from v0.4 still works the
same way; projects from v0.2, v0.3 and v0.4 open and are converted.

### New

**Subchannels: unlimited note groups inside a track**

* Any number of subchannels per track (1, 17, 1,000 …), each with its own
  colour and its own real MIDI channel (1–16) for new notes. Several subchannels
  can share a MIDI channel; PlumSpirit keeps them apart.
* **Subchannel strip** under the track list: the current subchannel and its MIDI
  channel, swatches for the first 16, **+** to add one (right-click **+** →
  *Add Subchannels…* to add many in one undo step) and a **#** field to jump to a
  subchannel by number (it offers to create one that does not exist).
* **Song → Subchannels…**: a searchable list of all of them (stays fast with
  thousands) with note counts — make current, rename, change MIDI channel
  (optionally moving its notes), select its notes, delete it with its notes
  (asks first), or move the selected notes into it.
* **Edit → Move Selected Notes to Subchannel…** regroups notes in one undo step.
* New notes go into the current subchannel; switching subchannels never
  recolours or moves existing notes.
* Copy / paste keeps each note's subchannel; pasting into another track creates
  the missing subchannels once.

**Subtracks: tracks inside tracks**

* Right-click a track → **Add Subtrack** (Ctrl+Shift+T) or **Add Subtracks…**
  (any number, one undo step). Subtracks can have their own subtracks, as deep as
  you like.
* Each subtrack is a full track: notes, events, subchannels, Track Properties,
  Mute / Solo, Onion Skin source.
* The track list is now a tree with ▸ / ▾ arrows. Collapsing a parent only hides
  rows — collapsed subtracks still play and export. The list stays smooth with
  thousands of rows.
* Deleting a track says how many subtracks and notes go with it and is one undo
  step. Right-click a parent for **Mute / Unmute / Solo Track + Subtracks**.
* The Conductor stays pinned at the top and can never be nested.

**MIDI import keeps the file's structure**

* Every MIDI track opens as a track; a track whose notes use several MIDI
  channels gets one subchannel per channel (single-channel tracks keep one).
  PlumSpirit never invents hierarchy a file does not contain.
* After opening, the status bar and **Details** show the mapping, for example
  *"Source MIDI: 3 track(s), 4 MIDI channel(s) used, 4 note(s). PlumSpirit
  result: 3 top-level track(s), 5 subchannel(s), 0 subtrack(s)."*
* Files exported by PlumSpirit come back exactly: subtracks, subchannels, names
  and every note's subchannel. If another program changed the file, that data is
  ignored safely and the status bar says so.

**Import MIDI Into Current Track** (File menu, Ctrl+Shift+I)

* Merges another MIDI file into the active track without replacing the project:
  * **Source tracks as subtracks** — each source track becomes a subtrack;
  * **Source tracks as subchannels** — each (source track, MIDI channel) pair
    becomes its own subchannel, so two tracks that both use channel 1 stay
    apart. Notes keep their real MIDI channels.
* Insert at the beginning or at the edit cursor.
* Exact PPQN conversion (for example 480 → 960); any rounding is reported before
  you apply.
* Conductor policy: **Keep** the project's tempo map (default), **Merge** the
  file's tempo / time signatures / markers (conflicts listed), **Replace**, or
  **Ignore**.
* Shows a summary and the notes to be added before Apply. Apply is one undo step;
  Cancel leaves the project, the undo history and the "unsaved" mark untouched.
  SMPTE and Format 2 files are refused with an explanation.

**Preview before Apply (on by default)**

* Scale X / Y, Quantize, Track Properties corrections, typing a new Gate or Vel
  for selected notes, and Import Into Track show their result in the piano roll
  before anything changes: changed notes as highlighted ghosts, notes that would
  stop sounding as red outlines, plus a short summary ("6 of 6 note(s) change")
  or the reason Apply would be refused.
* Updates live as you change values. Huge selections stay smooth: only notes near
  the view are drawn, and the full check runs in the background.
* Cancel changes nothing. What you previewed is exactly what Apply does, as one
  undo step.
* **View → Preview Batch Edits** (or the checkbox in each dialog) turns it off;
  Apply works the same without it.

**Onion Skin**

* **Ghost Opacity** from 0 to 100 % (slider or typed). At 100 % ghosts are fully
  opaque but slightly thinner, and they still can never be clicked or edited.
* **Layer order**: Project order, **Dense First** (the track with the most notes
  is drawn in front) or Sparse First, measured over the notes in view (default)
  or the whole track. This only changes drawing order, never the track list.
* Ghosts keep each subchannel's hue. Subtracks can be ghost sources.
* Opacity, layer order and sources are saved in the project.

**Playback and status**

* **Follow Position**: while playing, the view now keeps the playhead around the
  **middle** of the piano roll by default (v0.4 waited until the playhead was
  three quarters of the way across). **View → Follow Position…** sets 0–100 %;
  it is saved in your settings, not in the project.
* **Live Conductor BPM**: the Conductor row shows the BPM at the playhead
  (playing or paused) or at the cursor (stopped), including inside tempo ramps.
  Playback Speed never changes it; the tooltip mentions the speed.
* **Operation timings** in the status bar: "Last: Saved project — 23 ms",
  "Export failed after 2.17 s", "Import cancelled after 5.93 s". Running jobs
  show their elapsed time. Timed: open, save, import, export, Import Into Track,
  SoundFont loading, batch edits (paste, delete, quantize, transforms, add tracks,
  subtracks and subchannels), Select Range and subchannel selection. Single
  clicks and drags are not listed. **View → Recent Operations…** shows the last 20.

**MIDI export**

* The track tree is written flat and in order (parent, then its subtracks).
* Every note keeps its real MIDI channel; no channel number outside 1–16 is ever
  written, however many subchannels a track has.
* When several subchannels of a track share a MIDI channel, the export dialog can
  keep them apart in extra MIDI tracks ("layers", on by default) and add
  PlumSpirit data that other programs ignore and PlumSpirit uses to restore
  everything. The dialog explains the result before you export.
* Subchannel colours are PlumSpirit display only and are never written into MIDI.
* Projects with no subchannels or subtracks export **byte-for-byte the same** as
  v0.4.

### Changed

* **Project format**: `.lup` version 4 (track tree, subchannels, each note's
  subchannel, the new Onion Skin settings). v0.2, v0.3 and v0.4 projects open and
  are converted; saving writes version 4, which **older PlumSpirit versions
  cannot open**.
* **v0.4 projects** open as top-level tracks, and each track gets one subchannel
  per MIDI channel its notes use, so they look, play and export as before.
* **Note colours**: each subchannel has its own hue (subchannel 1 keeps the
  familiar colour); velocity still sets the saturation, and velocity 1 stays
  visibly coloured. The velocity lane uses the same hues.
* **Track Properties → Channel** is now the active subchannel's MIDI channel.
* Unnamed tracks keep their numbers when subtracks are added ("Track 2" stays
  "Track 2").
* New shortcuts: **Ctrl+Shift+T** (Add Subtrack), **Ctrl+Shift+I** (Import MIDI
  Into Current Track).

### Fixes and polish

* Dialogs settle their size as soon as they open, so nothing shifts under the
  mouse on the first move.
* The Export dialog keeps a fixed height, so checkboxes no longer move when an
  option changes the text below them.

### Performance and memory

Measured on a 2-vCPU Linux VM (release builds); details and commands in
`docs/PERFORMANCE.md`.

* Memory per note is unchanged for tracks that use one subchannel (20.24 bytes vs
  20.23 in v0.4). Tracks using several subchannels add 1 byte per note (2–4
  bytes only beyond 255 / 65,535 subchannels). No per-note objects were added.
* Preview never copies notes: previewing 1,000,000 selected notes adds 0.0 MB.
* 1,000 subtracks in one step: 0.24–0.33 ms. Ranking 1,000 Onion sources for
  Dense First: 0.08 ms.
* Importing a 1M-note file into one track: about 30 ms to prepare; Apply and
  Undo are instant for the subtracks mode.
* Cost to know about: `.lup` files with multichannel tracks are about 14 % larger
  and save about 28 % slower (≈ 25 ms more per million notes).

### Known limitations

* At extreme zoom-out, Onion ghosts are drawn in the track colour only;
  subchannel hues appear when zoomed in.
* Notes that start on the same tick in different export layers of one track may
  come back in a different order among themselves after a MIDI round trip (every
  note, time, channel and subchannel is kept).
* Opening a MIDI file shows its mapping afterwards rather than asking first;
  Import Into Current Track, which changes the open project, has full Preview.
* Conductor tempo points have no separate preview (they are edited directly and
  undoable).

### Testing

* 368 fast tests and 16 heavyweight stress tests pass (including 5,000,000 notes
  over 256 subchannels, 1,000 nested subtracks, and 1M-note Import Into Track and
  Preview). `cargo fmt` and clippy are clean.
* The full 78-step v0.5 workflow (contract §255) was run in the GUI on Linux; see
  `docs/TESTING.md`.
* Still to check on Windows by hand: items 22–30 of the manual checklist in
  `docs/TESTING.md` (native dialogs, WASAPI audio, Helvetica).

### Build

`PlumSpirit.exe` 0.5.0 (10,748,416 bytes, SHA-256
`786e9d2f2320ab2ccfa8b651697c02b4c437587b6c77de6e09b259134ba526c0`), built
2026-10-01 from commit `1f147e0` on branch `v0.5-dev`. Cross-compiled on Linux for
`x86_64-pc-windows-gnullvm` with llvm-mingw 20260922 (UCRT), the Rust 1.95.0
standard library built from source and LLVM's libunwind linked in; it needs only
DLLs that ship with Windows 10 and 11 (including the Universal C Runtime). A native MSVC build (`cargo build --release` on Windows)
gives an equivalent program.
