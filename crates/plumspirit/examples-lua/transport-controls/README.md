# Transport Controls

Install, grant `playback.read` and `playback.control`, and enable. Each Plugins command
requests one explicit native action: Play, Pause, Stop, exact-tick Seek or decimal
audition Speed. The actual native acknowledgement is shown and saved to this plugin's
own preset. No project/note or file access is requested. Native speed preferences
persist exactly as the existing toolbar does; project tempo and notes are unchanged.

Requests need the current job/project revision and native action generation. A user
seek, competing plugin action or project change rejects old requests. Native admission
is at most4actions/second per plugin and8globally; repeat invocations too quickly get
a clear busy/rate-limit error. Each worker has one pending slot, a32request job cap
and normal native memory/cancellation/deadline limits. No Lua runs in audio callbacks.
A successful receipt means the action was handled; it does not promise that a device
or SoundFont can play. The native state can be `audio_unavailable`.

Accepted actions are immediate and cannot be rolled back by canceling afterward.
Queued canceled/expired/revoked actions do nothing. Loading, lifecycle, observation,
services and automatic tool previews cannot request control. Loop range set/disable/clear uses the separate `stage_loop` completion API. The command
stages one root-free intent; the native owner applies it after successful completion
and snapshot release. Failure/cancel/stale/native-busy rejection discards it. The
native status and plugin log report refusal; a staged receipt is not proof of apply.

Loops persist as existing native project settings, mark the project dirty and are
outside musical Undo. Save the project normally to retain them. No notes are generated.
Do not combine a staged loop with musical transactions in one command. Other active
readers can cause a busy refusal; no note-store clone or hidden retry queue is used.
Precomputed MIDI scheduling remains a later mandatory requirement.
