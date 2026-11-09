# Editor Overlay Laboratory

Install through Plugins → Lua Plugins → Examples. Installation grants nothing. Enable
ui.overlay and the read permission for the requested surface. file.read_user_choice
is needed only for choosing a source image, through the native dialog.

Show the Piano Roll, then invoke Create Authored Checker Guide or Choose Image and
Author Luminance Mask. The selected file is closed after the existing native decoder
loads it. The Lua author thresholds luminance, colors the mask and places a translucent
guide using retained native UI pixels. Notes stay native and editable; no musical
notes, per-note RGBA storage, transactions or Undo steps are created by this package.

Frames belong to the exact captured project revision and view. Zoom, scroll, resize,
context changes or note edits discard a stale frame. Invoke Refresh Guide for Current
Roll View to republish the same retained image at the new captured view. Enable
Automatic Guide Refresh opts into the author's bounded surface_changed observer.
It republishes retained pixels using the exact new captured view on the Lua worker.
The native stale frame disappears until that bounded refresh is delivered.

Show MIDI Player and invoke Stage Captured Player Transport Readout to draw an authored
informational readout over its native compositor. It captures one transport snapshot,
without changing playback or replacing Player colors. Enable Automatic Player Readout
opts into surface/transport worker refresh; Disable Laboratory Automatic Refresh stops
both author refresh modes. Manual publications should be at least50ms apart. Automatic
refresh skips and logs host refusals rather than retrying in a loop. Project close
resets modes and releases the guide. Clear commands require the relevant visible
surface. Lua Plugins → Editor overlays independently disables a currently owned overlay.
Disable choices are session local and bounded. Disabling/reloading the plugin or closing
the project releases owned frames/images. Normal native upload and memory limits apply.

This is an actual API0.2 development example, not a full Windows/performance acceptance
claim. No Lua executes on paint/audio threads and no overlay intercepts pointer input.
