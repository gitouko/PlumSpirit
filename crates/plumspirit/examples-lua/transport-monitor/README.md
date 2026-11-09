# Transport Monitor

Install from Lua Plugins → Examples, grant `playback.read`, then enable.
The plugin needs no project/note editing authority. Change the native playhead, loop,
speed or playback state to receive bounded `transport_changed` worker notifications.
Use **Capture Transport Snapshot** from Plugins to show a captured run snapshot and
save it with the last observed state to the plugin's own preset. Tick values remain
Lua integers; the command uses the native snapshot captured when its job was queued.

Position notifications are coalesced state observations, not sample-accurate audio
callbacks or a complete transport event log. Native sampling is at most20Hz; worker
starts share the existing8Hz limit,32subscriptions/64pending entries and250ms run
budget. No Lua runs on the audio/render thread. This initial example controls no
playback and schedules no MIDI. Disable/reload/revocation/project changes invalidate
old jobs. Tables can be edited locally without changing the captured native state.
