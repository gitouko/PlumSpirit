# Text Art Studio

Install this example from Plugins → Lua Plugins → Examples (or install this folder),
then enable Project Read and Notes Edit for the original art tools. The new Color
Glyph Laboratory command additionally requests Installed Font Listing and Plugin
Panels through normal grant review. Original tools/commands keep their previous
permissions. No font files are redistributed.

Open a Piano Roll and choose Tools → Custom → Text Art Studio — Multiline Composition.
Enter up to 128 Unicode scalars over at most eight hard lines. Choose a bundled
Proportional, Monospace or Emoji face, or explicitly select Proportional + Emoji /
Monospace + Emoji. Choose base direction, left/center/right alignment,
cells per em, baseline spacing, first baseline, clipped time columns and fill/outline.
Start, step and gate use song ticks for a Track or local ticks for owned Clip content;
velocity and the existing native Track/Subchannel colors determine the musical result.
The actual 128/256-Key range determines rows. The first baseline counts down from the
top row; later baselines follow the chosen em spacing. Geometry outside the grid is
clipped. Save/Load preserves all parameters, including the original text and newlines.

Preview is private. Apply uses the native transaction and one Undo step; Redo restores
it. Cancel discards the pending preview. Linked Clip placements share their content,
as in native editing. Missing glyphs or oversized input fail without changing notes or
rewriting the source text. The original three font choices use one face exactly as
before. The two explicit stack choices try the selected text face first and bundled
Emoji second for each complete grapheme. No fonts are searched or opened implicitly.
This sample does not perform soft wrapping.

Every artistic algorithm is explicit Lua: bidi visual runs, logical script segments,
whole-grapheme font coverage and visual fallback subruns, shaped positions, line
alignment, baselines, original glyph contours, adaptive sampling, nonzero fill (which
preserves counters such as O), sparse cell union and batched note generation. A local
outline cache for each face and conservative clipping avoid repeated sampling of invisible glyphs.
At most 256 shaped glyphs and a 512×256 cell grid are processed. The host supplies
bounded worker execution, geometry and the note writer; it creates no art algorithms.

Nine existing commands remain available for proportional/monospace O and monochrome
😀 outlines/fills, and proportional/monospace `office é` and monochrome `😀🔥` shaped
art. Their original note counts and export fingerprints are regression tested in both
key modes. Installed-worker and native App harness tests exercise composition,
deterministic Preview/Apply, Cancel, Undo/Redo, linked Clip targeting and presets.
These are distinct from interactive Windows acceptance, which remains pending.

This is a development increment of the required Text Art Studio. The sample uses
bundled faces; the public font API separately supports bounded Windows-isolated
external single-run shaping. Contextual paired-punctuation typography, soft wrapping,
broader contextual/installed-font fallback, installed-font selector/rich UI, native dense
text processing and broader color-font
technologies remain separate requirements. CFF/CFF2/VARC plugin outlines are unavailable
until a bounded outline route exists.

Bundled coverage is tested individually: 😀/🇺🇸 have single covered glyphs; 👍🏽 has a
missing modifier, family emoji remains multiple components, and keycap remains
uncomposed. Preserved clusters alone do not establish composed appearance support.

The explicit fallback modes demonstrate mixed `AV😀A` and `π🔥` over separate lines.
Try `A😀A 🇺🇸` for a supported whole flag glyph. Proportional and Monospace remain
different primary outlines/metrics; glyph IDs and caches are face-local. Native
coverage is only a candidate choice: Lua still checks the actual shaped glyphs and
uses their original contours. The authored sequence policy requires one original
mapped glyph for flags, modifiers, keycaps and pictographic ZWJ sequences. Unsupported
`👍🏽`, `👨‍👩‍👧‍👦` and `1️⃣` fail clearly, preserving input and pending source notes;
they are not drawn as unrelated components. This is a documented sample policy,
not a claim that every one-glyph font mapping has the desired artwork.

Fallback choices and original text persist in the existing parameter presets.
Installed-worker and native App tests cover mixed text in both key modes, private
Preview, Cancel, Apply, one Undo/Redo, distinct fonts, linked Clip-local content and
DPI-scaled preset restore. The original nine commands and18output goldens remain.
These tests use real bundled glyphs and native headless frames, not a physical
interactive desktop or a complete color-font/Unicode acceptance matrix.

Open **Color Glyph Laboratory** from this plugin's command menu for a native preview
of original installed color-font layers. Enter an exact family/style (empty style
chooses the first face in that requested family), one original grapheme, and a
zero-based palette. The default is the configured Windows Segoe UI Emoji and 😀.
Press Preview explicitly. The author uses the existing isolated shaper, original
COLRv0 compatibility layers and bounded glyf contours; native adaptive flattening,
joint nonzero fill, A8 masks, colorization and composition produce one64×64native UI
image. Original layer order/alpha and one common aspect-preserving geometry mapping
are retained. Canvas resize events preserve a square preview at different DPI scales.
No font userdata is retained while waiting for panel interaction.

Inputs and displayed glyph are labelled separately. Failed input/font/format/budget
checks keep the previous image and original text. Save Inputs stages only plain
family/style/text/palette preferences; normal Close writes them through existing
atomic settings. Save/Load Preset uses an independent named plain input preset.
Loading a preset does not automatically render. Closing without Save leaves the old
preferences intact; native surface cancellation discards the running job. Musical
notes, Track/Subchannel colors and Undo are unchanged by this panel.

The example allows at most31original layers,4render/catalog attempts (including
failed attempts after input validation), and the existing cumulative64image/mask
creations. Each layer uses two resources and its composited output uses one; a new
preview is refused before exceeding that bound. There is only one UI image per
preview, within the unchanged16UI-image creation limit. Close/reopen for a fresh
job when its allowance is used. Geometry has explicit point/contour/work caps;
binary64pixel coverage is not an antialiased font rasterizer.

Actual configured Windows Segoe UI Emoji's 😀 was tested through6original v0layers,
including real installed-worker/native panel rendering and texture upload. Controlled
font files independently test3layer/palette/counter paths, failed-preview preservation,
resize aspect, plain presets, cancellation and budgets. Native headless App tests
cover both key modes/DPI1–1.5, Save/reopen and unchanged musical document/Undo. These
are not a physical interactive desktop or peak-RSS acceptance claim. COLRv1 paints,
CFF/CFF2/VARC/bitmap/SVG execution and universal emoji appearance are not provided by
this laboratory. The original9commands and18output baselines remain; this is the
tenth command, while the original multiline composition tool stays available.


Version0.4 adds **Capture for MIDI** to the color laboratory and a second Piano Roll
custom tool, **Color Glyph Artwork**. Preview an installed glyph, then Capture for
MIDI and Close normally. The displayed image, including a retained previous preview
after invalid inputs, is the capture source; its own family/style/text/palette label
is stored. This explicitly stages64×64straightRGBA pixels as32KiBhex plus plain
source metadata in this project's namespaced plugin settings. No font file, path,
handle or permission is stored. It marks the project changed outside musical Undo;
Canceling the running panel discards staged project data. Existing input preferences
and input presets are independent of the captured pixels.

Save Pixel Preset writes the displayed pixels to **Color Glyph Laboratory Pixels**.
Load Pixels Into Project stages that fixed preset without loading a font or changing
the displayed preview. Close normally to publish the project capture. Pixel presets
contain the same bounded versioned plain data and can be restored even if the font
is unavailable; obey the font's license when sharing rendered artwork.

In Tools > Custom choose **Color Glyph Artwork**. The author's Lua resizes the fixed
source to1–64time columns and pitch rows, clips rows at the actual128/256key ceiling,
and quantizes to the first1–64existing Subchannels of the actual editing owner. It
reads their native unselected fill colors at the selected velocity, using the run's
palette and theme. Nearest-color ties, alpha threshold, pixel notes versus horizontal
equal-color runs and exact integer tick mapping use public native image note plans.
The plugin chooses all mapping/options and passes explicit Subchannel handles to the
transaction-owned note writer. No Subchannels are created, palette changed or per-note
RGBA invented. Existing note colors remain authoritative and may change with native
palette preferences after generation. The preview source is fixed; Apply never
reopens fonts, dialogs or image files. It needs only project.read/notes.edit.

Track and Subtrack edits use song ticks; owned and linked Clip edits use content-local
ticks and the actual editing owner's palette/MIDI channels. Linked content remains
shared. Preview changes nothing; Apply is one native Undo step, Cancel discards it,
and Redo restores it. Tool sizing/placement/velocity settings use normal native tool
presets; the project capture is separate and persists in LUP. Invalid capture/pitch
or failed processing changes no notes. At most4096pixel notes can be emitted by this
sample. Native host sandbox, writer quotas and cancellation remain in force.

Staged project plugin settings invalidate retained tool results and their worker
generation. New tool runs and Apply wait until the data can be persisted; an old
worker reply cannot restore an old capture's preview. Native App tests hold a reader
open while replacing a pixel preset, then verify fresh preview/Apply/one Undo/Redo,
128/256keys, DPI1–1.5 and LUP. Installed-worker tests use controlled fixed pixels to
verify exact ticks above2^53, native RGB/Subchannel/MIDI routing on Tracks/Subtracks/
linked Clips, both note modes, invalid input recovery, and operation without font/UI
grants. Controlled font tests verify exact RGBA capture/preset reconstruction and
cancellation after staging. These are Windows native/headless tests, separate from
physical interactive desktop, peak-RSS and final release acceptance.
