# MIDI Pattern Laboratory

Install this folder from Lua Plugins, or install the embedded example. Installation
grants no capabilities. Enable the separately described `playback.read` and
`playback.emit_midi` permissions to use its explicit commands.

Load a native SoundFont, use Realtime playback, and start a project with at least
four seconds remaining. Run **Stage Eight-Note Pattern**. The author-written Lua
arpeggio starts about1.5seconds after the captured position; native ramp/speed/SMPTE
mapping determines each tick. It uses isolated internal MIDI voices, independently
of song/audition voices, and changes no notes, `.lup`, musical history or external
MIDI routes. Root, intervals, timing, velocity and ordering are explicitly authored.
The root/interval preset belongs to this plugin; it is not a persisted live schedule.

Staging is not an output acknowledgement. **Inspect Native MIDI State** reports
`queued`, `running`, `finished`, `canceled` or `refused` for the native plugin stream.
Only one plugin stream can run globally. A late/full/out-of-project/changed native
output refuses the plan; no hidden retry or automatic Play occurs.

**Cancel My MIDI Stream** affects this plugin's accepted/pending stream, preserving
song playback. Pause, Stop, seek, speed/tempo/project/output/loop changes, loop wrap,
plugin disable/reload/removal/revocation and global script cancellation clean up its
owned voices. Previously mixed audio may remain for the native ring lookahead.
Subsequent read-only inspections do not restart or cancel an accepted stream.

This example requires the API0.2 development scheduling implementation. Native
headless synthesis/integration evidence and actual Windows/hardware acceptance are
tracked separately in the development records.
