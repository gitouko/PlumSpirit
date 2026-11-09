# Image Art Studio — API0.2 example

This installable plugin contains the Lua author's artwork and editing algorithms.
The host provides image/mask resources, native note plans/writers/transactions and
its existing parameter-tool interface. No advanced-function catalog is generated.

Install from Plugins > Lua Plugins > Examples, review the declared project.read,
notes.edit, file.read_user_choice and ui.panels capabilities, then enable the plugin.
All file dialogs occur only in the explicit Choose Image command. Tools never reopen
a path or prompt during automatic Preview. No network/executable/device access.

1. Open a project. Optionally run **Image Art Studio: Choose Image...** from the
   plugin command list. Cancel leaves the source unchanged. Shared native decoding
   reads only that chosen file. The author downsizes it to at most64x64 with the
   native box filter and stores at most32KiB of ASCII RGBA hex plus dimensions in
   this project's plugin settings. These are captured pixels, not a file path,
   resource token or continuing permission to read the source file. Saving the LUP
   keeps this bounded capture. Reset removes it and restores the authored default.
2. **Show Captured Source** displays those pixels through the existing declarative
   image/canvas UI. This command makes no musical edit. Close it before starting a
   tool. With no capture the Lua code creates its own striped/checkered image.
3. Use **Tools > Custom > Image Art Studio — Note Artwork**. Pick the image domain,
   1–64time columns/pitch rows, tick step, velocity and authored palette. Width/height
   are explicit author choices; rows clip to the actual128/256key mode. Equal-color
   runs or individual pixels feed image:note_plan then writer:append_plan directly.
   Inspect native Preview before Apply; Cancel changes nothing, Apply is one Undo.
4. **Image Art Studio — Mask Editing** uses selected notes, or the whole native
   target when nothing is selected. Choose alpha silhouette, luminance, brightness
   or a Lua-chosen green key, plus start/center/any/all/average coverage. The author
   chooses velocity, gate, transpose, shift, deletion, duplication or selection.
   Actual editing uses tx:mask_notes and the existing indexed note/Undo machinery.
   Native Preview/Apply use the same captured source and parameters. Destructive
   changes remain subject to native output/memory limits and confirmation. Selection
   replaces selection only and follows native selection-only no-musical-Undo behavior.

Targets are the actual current native tool target: Track, Subtrack or owned Clip
content. Track ticks are song ticks; owned content ticks are local and affect linked
placements. The existing native context binds ownership/default channel/key mode.
The artwork tool cycles existing native Subchannel handles on Track/Subtrack targets
when routing is enabled. With one Subchannel all palette entries share that native
color. Owned Clip content deliberately uses its native host's main Subchannel/default
channel. The backend also supports explicit owned-Clip routing; this sample does not
infer an owner Track from a content ID. Notes never store arbitrary RGB/RGBA colors.
Image RGB controls authored palette classification; display colors remain native.
Keys above127 retain the existing explicit Experimental256-Key MIDI export requirement.

Save/load parameter presets in the existing native tool interface. **Save Capture
Preset** and **Load Capture Preset** separately save/restore the bounded plain captured
pixels (or authored-default marker) in the plugin's own preset directory. Capture
settings are project settings outside musical Undo, following the existing API0.1
config contract. No resource/file grant is serialized. Changing the source invalidates
the old project snapshot; close/reopen a tool and Preview again after a source change.

Malformed capture/preset data is rejected before notes change. Native group/ancestor
locks, supported Subchannels, actual key modes, Clip ownership, cancellation and
transaction rules apply. A write-phase failure closes/discards the entire private
transaction; preprocessing failure changes nothing. These small64x64demonstrations do
not imply unlimited image sizes or unbounded note generation. The public primitives
are independently documented in docs/LUA_API.md and support their stated native limits.

Source/native worker/application tests and actual Windows behavior are recorded
separately under docs/v1_0_1_api02/evidence. No full Windows/dense/RSS/release compatibility
claim follows merely from installing or parsing this package.
