# Reactive Velocity Guard

Install this folder in Lua Plugins, grant Project read, Project observation,
Automatic project note edits and Note edits, and enable it. On a subsequent content
change the author-written observer offers the registered `clamp` command. The separate
worker clamps velocities above 100 on the **first Track** to 100 in one Undo step.
Use Run → Clamp First Track Velocity for an ordinary explicit invocation. The example
performs no edits without the ordinary note-edit permission. Existing group locks apply.

Undo restores the user's edit and suppresses automatic reapplication; Redo restores
the same staged result. Automatic edits do not trigger more automatic edits in this
or another plugin. A later external musical edit can begin another response. Shared
500 ms admission spacing and lossy coalescing mean this is a bounded state reaction,
not a guarantee that every input edit receives a response.

Automatic jobs currently support note edits only (including existing packed note
metadata/group assignment); structural, Conductor, controller-event, group-table,
explicit selection and project-config changes require an explicit command. Limits:
one committed transaction, eight native operations, 100,000 created/changed/removed
notes, 4,096 Track/owned-content stores, 250 ms including snapshot preflight/copy,
shared native memory quota. Conservative copy/history reservations may refuse a large
project below other caps. Large explicit commands retain their existing API 0.1 budgets.
Failures pause observations until reload; the explicit command remains usable.

This is demonstration Lua, not an automatically generated host function. Change the
rule or algorithm in the source to author another reactive tool. Source-level and
native headless integration tests cover installation/Apply/Undo/Redo; actual interactive
supported-target Windows acceptance remains part of the release gate.


This package is also embedded in **Plugins > Lua Plugins > Examples**; no download
or source checkout is needed. Installation starts disabled with no capabilities
granted by default. Review the listed permissions and enable it using the normal
plugin controls. Reinstall restores the shipped files and preserves your decisions.
