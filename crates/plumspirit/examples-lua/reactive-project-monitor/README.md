# Reactive Project Monitor

Install this folder from Settings → Lua Plugins and grant Project read and
Project observation, then enable it. Edit/select notes, modify Tracks or Clips,
change the Piano Roll zoom, Undo/Redo and save. The bounded plugin log records
`OBS <kind>` metadata; Run → Show Observation Summary prints per-kind delivery
counts and a 16-entry recent-signal count. No musical content is changed.

Signals are coalesced invalidations, not an audit log. `dropped` is cumulative
bounded-queue loss for this runner. No startup replay is guaranteed: the ordinary
API 0.1 `on_project_open` hook initializes state. Selection counts include stored
Track and owned-Clip note selections; Clip placement counts count explicit
placements. Viewport origin is exact unsigned decimal text. Read-only event jobs
receive no project handle; an explicit command can obtain a normal read snapshot.
Save signals mean successful native completion, not a retained file snapshot.
Disabling/reloading the plugin clears native subscriptions and in-flight authority.


This package is also embedded in **Plugins > Lua Plugins > Examples**; no download
or source checkout is needed. Installation starts disabled with no capabilities
granted by default. Review the listed permissions and enable it using the normal
plugin controls. Reinstall restores the shipped files and preserves your decisions.
