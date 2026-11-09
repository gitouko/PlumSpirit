# MIDI Input Laboratory

An API 0.2 demonstration of author-written capture, persistence and mathematical
mapping. Install it from **Plugins → Lua Plugins… → Examples**, enable it and grant
`midi.input`, `project.read` and `notes.edit`. Installation does not approve a device:
**Capture** always uses the native device chooser and requires explicit approval.
Windows WinMM short messages only; the development machine currently reports no
physical inputs, so manufactured/headless tests are separate from hardware acceptance.

1. Open a project and run **Capture MIDI Input**. Choose and approve your device,
   then play/send MIDI. Capture stops at 256 messages or after 32 reads with up to
   100 ms wait each. The command never routes live sound or writes musical notes.
2. Save the LUP project to persist the exact captured columns. **Inspect** reports
   the source/count; **Save/Load MIDI Capture Preset** copies the bounded schema.
   Presets retain data, never device access. **Clear** removes the project capture.
3. Open **Tools → Custom → MIDI Input Laboratory → Captured Note Art**. Choose
   Notes, CC Art or Bend Ribbon, Preview, then Apply or Cancel. Apply uses the
   native transaction and one musical Undo/Redo boundary. Start is song-relative
   for Tracks and local for Clip content; existing ownership/locks still apply.

Every mapping is explicit in `main.lua`. Notes uses FIFO pairing for repeated
same-key/channel Note Ons, zero-velocity Note On as Off, and author handling for
All Sound Off/All Notes Off/System Reset. Unfinished notes use the chosen gate;
late release timestamps get an explicitly chosen one-millisecond minimum gate.
This is not a complete performance recorder: sustain pedal and every MIDI device's
musical conventions are not interpreted. The original timestamps/order remain stored.
CC Art maps the selected controller value to a key span. Bend Ribbon maps signed
14-bit bend to a pitch offset. Both use the chosen gate/velocity. Exact integer tick
arithmetic uses the minimum captured timestamp as time zero across all message
classes, then the selected integer ticks per millisecond. It rejects overflow, and Error/Clamp/Drop makes out-of-range pitch policy
explicit. Preserving MIDI channels changes MIDI channel metadata, not Subchannels
or per-note RGBA: destination defaults and native Track/Subchannel colors remain.

Preview/Apply reads a fixed captured project snapshot, never a live pointer/device
or wall-clock random state. Editing the capture requires a new preview. A canceled
or failed capture preserves the previous recording; any queue/rate/invalid/contention
or driver-overload indication rejects the new recording. Capture errors/Cancel/job
expiry close live access. Successful capture/preset settings use the existing settings
persistence path, outside musical Undo; replayed note changes use native Undo.

Limits and live-input lifecycle are in `docs/LUA_API.md`. Native code provides bounded
input, approved devices, geometry/editing services and transactions; it does not invent
the recording or artwork algorithms shown here. No automatic reconnect, SysEx, raw
OS input hooks, audio-thread Lua or built-in advanced effect is added.
