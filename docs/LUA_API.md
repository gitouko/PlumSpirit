# PlumSpirit Lua scripting (v1.0)

**API version:** `0.1.0` in the supplied v1.0 source. This scripting API was introduced during v1.0-beta1 and continues in v1.0. The reference below documents the current implementation, including notes, events, Tracks, Clips, Preview/Apply transactions, capabilities, custom tools, presets, and examples. The API version is deliberately separate from the app version; check `plum.api_version` rather than assuming it equals `1.0.0`.

New to plugins? Install an [example](#examples) from **Plugins → Lua Plugins…**, read its
`main.lua` in the plugins folder, then come back to the section of each call it uses.

## Runtime

* **Lua 5.4.9** (Lua.org, PUC-Rio, MIT licence), compiled into PlumSpirit.exe from source
  (the `lua-src` crate) — no system Lua, the same interpreter on every PC.
* Embedded through **mlua 0.12** (MIT). Licences: `THIRD_PARTY_NOTICES.md`,
  `THIRD_PARTY_LICENSES.txt`.
* Every community script is a `.lua` text file. Binary (precompiled) chunks are refused:
  they can crash the interpreter.
* `plum.lua_version` is `_VERSION` ("Lua 5.4").

## Versions

| Field | Meaning |
|---|---|
| `plum.version` | PlumSpirit's version, e.g. `"1.0.0"` |
| `plum.api_version` | the scripting API's version, `"major.minor.patch"` (now `"0.1.0"`) |
| `plum.api.major` / `.minor` / `.patch` | the same as integers |
| `plum.require_api(major [, minor])` | `true` when this PlumSpirit runs a script written for `major.minor`, else a **compatibility** error saying which version the script needs |

Semantic versioning: a **patch** changes nothing scripts can see; a **minor** only adds
(new functions, fields, capabilities); a **major** may change or remove things. A script
written for `major.minor` runs on every host with the same major and an equal or later
minor. While the scripting API major version is 0, it may still change between minors; changes are
listed in CHANGELOG.md. A script should start with `plum.require_api(0, 1)`.

## Capabilities

Outside the sandbox a plugin can only do what the user granted it, per plugin (the
plugin's settings; the manifest asks for them). Names are stable:

| Capability | Allows |
|---|---|
| `project.read` | reading tracks, clips, notes, events, the Conductor and the selection |
| `notes.edit` | changing notes and controller events (through Preview / Undo) |
| `tracks.edit` | adding, removing and changing tracks, subtracks, folders and clips |
| `conductor.edit` | changing tempo, meter and markers |
| `file.read_user_choice` | reading a file the user picks in a dialog |
| `file.write_user_choice` | writing a file the user picks in a dialog |

`plum.capabilities()` returns every name with `true` / `false` (granted or not);
`plum.has(name)` answers one. An API function needing a capability that was not granted
fails with a **capability** error naming it — before doing anything.

## The sandbox

Loaded: `table`, `string` (without `string.dump`), `math`, `utf8`, `coroutine`, and the
base library without `dofile`, `loadfile` and `require`; `load` reads text chunks only;
`collectgarbage` allows `"collect"`, `"count"`, `"step"`, `"isrunning"`; `print` writes to
the plugin's output in PlumSpirit (never a console); `os.clock()` is the only `os`
function.

Not available: `io`, the rest of `os` (`execute`, `remove`, `rename`, `getenv`, `exit`,
`tmpname`, …), `package` / `require` / native (C) modules, `debug`, network, processes,
native memory.

## Limits

Every run has:

* a **memory quota** (default 256 MB for the plugin's Lua state; allocations past it fail
  with a **memory** error),
* a **time budget** (default 10 s), checked every 10,000 Lua instructions; a command or a
  custom tool the user started (it shows a running indicator with **Cancel**) may run up to
  10 minutes, so a batch over millions of notes can finish,
* an optional **instruction budget** (Lua instructions per run, counted in steps of
  10,000; past it the run stops with a **budget** error),
* an **output budget**: notes a run's transactions may create — inserted, changed or
  duplicated (default 50,000,000) — checked **before** any note is built, so an
  exploding script is refused with a **budget** error instead of exhausting memory, and
* **cancellation** from PlumSpirit (the user's Cancel, or PlumSpirit closing — closing
  raises every running script's cancel flag and never waits for one; the app keeps its
  hosts in a `lua::Scripts` registry).

The host checks the time and the cancellation between instructions. A single call into a
library function (a huge `string.rep`, a pathological `string.find` pattern) runs to its
end before the next check; the memory quota still bounds it. Output-note counts are
estimated before any note is created (the API slices).

## Errors

Every failure is reported as a category, the script file and line when known, and a
message, e.g. `Error in my_tool.lua, line 12: attempt to index a nil value (local 't')`.

| Category | When |
|---|---|
| `syntax` | the script does not parse |
| `runtime` | an error while running (`error(...)`, a nil index, a wrong argument to a PlumSpirit function) |
| `memory` | the memory quota was reached |
| `time_limit` | the time budget ran out |
| `canceled` | the user or PlumSpirit stopped it |
| `capability` | something the plugin was not granted (or `collectgarbage("stop")`) |
| `compatibility` | the script needs a newer API (`plum.require_api`) |
| `expired` | a handle whose object no longer exists, or a note / event handle whose notes changed ([Handles](#handles)) |
| `internal` | a PlumSpirit defect inside a script call; nothing was changed and the plugin keeps working |
| `budget` | the run's instruction budget or note output budget was used up |

An error raised inside a PlumSpirit function names the script line that called it.

No Rust panic crosses into PlumSpirit: a failing host function becomes an `internal`
error. Edits a script asks for are collected and applied only when it finishes
successfully, one Undo step per committed transaction — a failed run changes nothing
([Transactions](#transactions)).

## Numbers

Lua 5.4 integers are 64-bit. Every tick, count, id and MIDI value PlumSpirit gives a
script is an **integer** (`math.type(x) == "integer"`), never a float; floats only for
display values (seconds, percentages). PlumSpirit stores ticks as unsigned 64-bit numbers,
so the range a script can hold is 0 – 2⁶³ − 1 (`plum.ticks.max`, also
`plum.max_integer`); a document value past it (unreachable in practice) is an error,
never a wrapped or rounded number.

Where a function takes a tick it accepts an integer, a float that is a whole number no
larger than 2⁵³ (`plum.ticks.exact_float_max` — above that a float may already have lost
precision, so it is refused), or a decimal string (`"4_611_686_018_427_387_904"`).

Lua's own integer arithmetic **wraps around** silently on overflow (`(1 << 62) * 2` is
negative). `plum.ticks` computes with checks and fails with a **runtime** error instead:

| Function | Returns |
|---|---|
| `plum.ticks.check(t)` | `t` as an integer tick, or an error (negative, not whole, a float above 2⁵³, past 2⁶³ − 1, not a number) |
| `plum.ticks.parse(s)` | the decimal string `s` as an integer tick (`+` and `_` allowed) |
| `plum.ticks.add(a, b)` / `sub(a, b)` / `mul(a, b)` | the exact result; an error past 2⁶³ − 1 or before tick 0 |
| `plum.ticks.muldiv(a, num, den [, mode])` | `a × num ÷ den` computed in 128 bits, rounded by `mode`: `"floor"` (default), `"ceil"` or `"round"` (halves up); an error when `den` is 0 or the result is past 2⁶³ − 1 |

## Handles

Scripts reach PlumSpirit's objects through **handles**, never pointers or list indexes.
A handle names one object of one open project: the project, the object's stable id, and
the id's *incarnation*. Every use checks it against the project as it is now.

| Handle (`kind()`) | Names | Stable id |
|---|---|---|
| `project` | the open project (`ProjectId`) | a number for each project opened or created in this run |
| `conductor` | the project's single Conductor (tempo, meter, markers) | — |
| `track` / `subtrack` | a Track; a subtrack is a track with a parent (`TrackId` / `SubtrackId`) | the track id (survives moving, renaming, re-nesting) |
| `folder` | a Track Folder | the folder id |
| `clip` | a clip placement on a track (`ClipId`); a track's primary clip is its own content | the clip id |
| `clip_content` | a clip content (`ClipContentId`); linked clips share one | the content id |
| `subchannel` | a logical subchannel of a track | track id + subchannel id |
| `note` | one note of a track's or a clip content's notes (`NoteId`) | its place in the notes, at one note state |
| `event` | one controller / pitch bend / program change / other preserved event | its place in the events, at one event state |

Every handle has:

* `h:kind()` — the name above (`"subtrack"` when the track has a parent now);
* `h:key()` — a string naming the object, equal for every handle to the same object and
  different for any other (use it as a table key: handles are new userdata each time, so
  the handles themselves are not table keys);
* `h:valid()` — `true` while the object exists;
* `h:check()` — `h` itself, or an **expired** error;
* `a == b` — the same object; `tostring(h)` — its key.

**Freshness.** A handle is **expired** when its object was deleted, when its creation was
undone, or when it belongs to a project that is no longer open; `h:valid()` is then
`false` and every function that needs the object fails with the **expired** category. An
expired handle stays expired even if Undo brings the object back — clip, content, folder
and subchannel ids can be handed out again (after an Undo, or the lowest free subchannel
id), and a handle must never silently reach a different object that now has the same id.
Take a new handle from the restored object.

**Notes and events** have no ids in PlumSpirit — millions of notes are stored in shared
chunks without a per-note record. Their handles are **snapshots**: valid while the notes
(events) of their track or clip content are exactly as they were when the handle was
taken (a selection change does not count). Any change of those notes — by the user, Undo
or a committed script — expires them; read the notes again. (Within a script's own
transaction the transaction API keeps track of the notes it changes — B1-S54.)

Selections are handed out as handles of the selected objects (Selection API, B1-S55).

How it works: the undo history keeps a ledger of how often every track, clip, content,
folder and subchannel id disappeared (it compares the ids after every step; nothing is
saved). A handle records the incarnation of its id; it resolves only while the id exists
with the same incarnation. Note and event handles also record the note (event) state
number — never reused within a run — and the values at their place.

## Coordinates

| Quantity | Domain | Notes |
|---|---|---|
| **Project ticks** (song ticks) | integer 0 – 2⁶³ − 1 | the timeline of the whole project, from the start of the song; the Track List and the Piano Roll show these |
| **Track List (absolute) ticks** | = project ticks | a clip at `start` covers song ticks `[start, start + length)` |
| **Clip-local ticks** (content ticks) | integer ≥ 0 | positions inside a clip content; a clip shows content ticks `[offset, offset + length)` at song tick `start`: `song = local − offset + start`, `local = song − start + offset` |
| **PPQN** | 1 – 2,147,483,647 in a project (native); 1 – 32,767 in a Standard MIDI file; SMPTE timing has frames × subframes instead | ticks per quarter note of the project (export rescales a native PPQN above 32,767 and says so); `ticks per bar = PPQN × 4 × numerator ÷ denominator` |
| **Tempo-map time** | seconds (float, display only) | physical time through the Conductor's tempo map (steps and ramps); tempo is global — one map for the whole project |
| **Keys** | 0 – 127 (MIDI); 0 – 255 in 256-Key Mode | 60 = middle C (C4); keys above 127 exist only in 256-Key Mode projects; a Standard MIDI file cannot hold them (export's 256-key policy decides, and says so) |
| **Channels** | 0 – 15 in the API | shown as 1 – 16 in PlumSpirit; channel 9 (shown 10) is the General MIDI drum channel. A note keeps its own channel: tracks are not channels |
| **Subchannels** | id 0 – 4,294,967,295 per track | logical note groups, not MIDI channels; shown as number `id + 1` ("Subchannel 1" is id 0); each has a MIDI channel for new notes; no other limit than memory |
| **Velocity** | 1 – 127 for notes | 0 would be a Note Off; note-off velocity 0 – 127 |
| **Gate** (note length) | ticks ≥ 0 | a note ends at `start + gate` |
| **Controller (CC)** | number 0 – 127, value 0 – 127 | program change 0 – 127, channel pressure 0 – 127, poly pressure key + value |
| **Pitch bend** | −8192 – +8191 (0 = center) | raw 14-bit value 0 – 16,383 (8,192 = center) |
| **Hierarchy** | Conductor · Tracks · Subtracks · Folders · Clips | one Conductor per project (never deleted); tracks form a depth-first tree (a subtrack is a child track, subtrees contiguous); folders group top-level tracks only; every clip sits on one track and shows one content (a track's own notes, or an owned content that linked clips share) |

## Project

Needs `project.read` (as does every read through a handle).

| Call | Returns |
|---|---|
| `plum.project.current()` | the open project's handle, or `nil` when no project is open |
| `p:name()` | the project name |
| `p:file()` / `p:file_type()` | the file path (or `nil` when never saved) / `"lup"`, `"midi"` (an imported MIDI file; Save asks for a `.lup`) or `"new"` |
| `p:dirty()` | `true` with unsaved changes |
| `p:revision()` | an integer that changes with every edit, Undo and Redo |
| `p:format()` | the Standard MIDI File format export writes: 0, 1 or 2 |
| `p:ppqn()` / `p:division()` | ticks per quarter note (`nil` for SMPTE timing) / `{kind = "ppqn", ppqn = 960}` or `{kind = "smpte", fps = 25, ticks_per_frame = 40}` |
| `p:key_range()` | 128, or 256 in 256-Key Mode |
| `p:duration_ticks()` / `p:duration_seconds()` | the song end (what the clips play, the Conductor, explicit ends) |
| `p:seconds_at(tick)` | seconds from the start through the tempo map (float) |
| `p:track_count()` / `p:note_count()` / `p:selected_note_count()` | counts (notes as the clips play them) |
| `p:content_hash()` | a hex string hashing everything the exported MIDI file depends on (not the selection, Mute / Solo or view) |
| `p:tracks()` / `p:track(n)` / `p:folders()` | every track in list order / the track at list position `n` (1-based) or `nil` / every folder |
| `p:conductor()` | the Conductor's handle |
| `p:tempos()`, `p:time_signatures()`, `p:markers()` | the same as on the Conductor |

**Conductor** (`c = p:conductor()`): `c:tempos()` → `{tick, bpm, bpm_float, micros, ramp, implicit}`
per tempo point (`bpm` is the exact decimal as a string, e.g. `"120"` or `"143.5"`; `micros`
the MIDI tempo in microseconds per quarter note; `ramp = true` when the tempo changes
linearly to the next point; `implicit` for the 120 BPM a file without a tempo assumes);
`c:time_signatures()` → `{tick, numerator, denominator, implicit}`; `c:markers()` →
`{tick, end_tick (chapters only), title, chapter, cue}`. Tempo is global: there is no
per-track tempo.

## Tracks, subtracks and folders

`t:kind()` is `"track"` or `"subtrack"`; the Conductor is not a track (`p:conductor()`).

| Call | Returns |
|---|---|
| `t:name()` / `t:label()` | the track's own name (`""` when none) / what the Track List shows ("Piano", "Track 3", "Subtrack 2") |
| `t:id()` | the stable track id (integer) |
| `t:index()` / `t:depth()` | list position (1-based, depth-first) / number of parents (0 = top level) |
| `t:parent()` / `t:children()` | the parent track or `nil` / the direct subtracks in order |
| `t:folder()` | the folder of the track's top-level track, or `nil` |
| `t:color()` | `{r, g, b, custom}` — Track Properties' color, else the automatic color of the current theme |
| `t:mute()` / `t:solo()` | playback Mute / Solo |
| `t:collapsed()` / `t:visible()` | its subtracks are folded away in the Track List / it is shown (no folded parent, not in a folded folder) |
| `t:note_count()` / `t:stored_note_count()` / `t:clip_count()` | notes the track plays (through its clips) / notes stored in the track's own content / clips on the track (the primary clip included) |
| `t:default_channel()` | the MIDI channel (0 – 15) of new notes |
| `t:subchannels()` | its subchannel handles |

**Subchannels** (`s`): `s:id()` (0-based, stable), `s:number()` (shown: `id + 1`), `s:name()`,
`s:channel()` — the MIDI channel (0 – 15) its new notes get, `s:track()`. This is the
logical → export mapping: each note keeps its own MIDI channel and exports on it; a
subchannel is a note group with a color, not a MIDI channel.

**Folders** (`f`): `f:id()`, `f:name()`, `f:tracks()` (its top-level tracks in list order),
`f:collapsed()`. Folders group top-level tracks only.

A method of another family fails with a **runtime** error that says so
(`mute() is not available on a folder handle.`).

## Clips

Clips are the Track List's clips — the same model, edited through the same planners
(`editor/clips.rs`, `editor/clip_merge.rs`), never a second playlist. A clip is a
**placement** (where a content plays: track, song start, the window of content ticks it
shows, mute, name, color) of a **content**: the track's own notes for a track's primary
clip, or an owned content that linked clips share. Clip ids are stable: a handle stays
valid across moves, resizes and renames and expires when the clip is deleted (or its
creation is undone). Reads need `project.read`; writes need **`tracks.edit`**.

**Finding clips**

| Call | Returns |
|---|---|
| `t:clips()` / `t:clips{start_tick =, end_tick =}` | the track's clips (the primary first, then by start), optionally those meeting `[start_tick, end_tick)` in song ticks |
| `t:clips_at(tick)` / `p:clips_at(tick)` | the clips playing at a song tick |
| `p:clips{track =, start_tick =, end_tick =}` | every clip of the project (or of one track) in list order |

An empty clip covers no tick, so a time filter never finds it.

**A clip** (`c`)

| Call | Returns |
|---|---|
| `c:track()` | the track it sits on |
| `c:start_tick()` / `c:end_tick()` / `c:song_length()` | its song span `[start, end)` |
| `c:offset()` / `c:window()` | the content tick shown at the start / the content ticks `from, to` it shows (`to = nil`: to the content's end) |
| `c:length()` | the window length, or `nil` when the clip shows its content to the end (it grows with it) |
| `c:content()` / `c:content_kind()` | the content handle — the track itself for a primary clip (`"track"`), else a clip content handle (`"owned"`) |
| `c:is_primary()` | the track's own clip |
| `c:linked_count()` / `c:linked_clips()` | placements showing the same content (this one included) / the others |
| `c:to_song(local)` / `c:to_local(song)` | exact conversion: song = local − offset + start (an error before the clip / its window) |
| `c:contains(song_tick)` | the tick is inside the clip |
| `c:mute()` / `c:name()` / `c:color()` | mute (a muted clip is shown but does not play, export or count) / its name (`""`) / `{r, g, b, custom}` (its own color, else the track's) |
| `c:notes{...}` | a note query on the content **inside the window**, in content ticks; writes through it change the content — every linked clip shows them |

**Clip edits** (in a transaction; each is one operation of the transaction's Undo step)

| Call | Does |
|---|---|
| `tx:add_clip(track, start, length)` | a new empty clip (its own content); returns it |
| `tx:move_clips(clips, {shift = n \| start = tick, track = t})` | moves the clips together (`start` places the block's first clip; the others keep their distance; `track` puts them all on one track) |
| `tx:duplicate_clips(clips [, {linked = true, start =, shift =, track =}])` | copies placed right after the block (like Ctrl+D) or where asked; `linked = true` (default) shares the content, `false` makes independent copies; returns the new clips |
| `tx:resize_clip(c, {start =, offset =, length =, end_tick =, open_ended = true})` | `start` moves the left edge with the notes kept where they play; the right edge stays unless `length` / `end_tick` / `open_ended` is given |
| `tx:slice_clips(clips, tick)` | each clip the tick falls inside becomes two showing the same content; returns the right parts (the left parts keep their handles) |
| `tx:set_muted(clips [, muted = true])`, `tx:rename_clip(c, name)` (or `tx:rename(c, name)`), `tx:recolor_clips(clips, {r, g, b} \| nil)` | mute, name, color (`nil`: the track's) |
| `tx:delete_clips(clips)` | removes them; a primary clip's notes stay with the track, an owned content goes with its last clip (counted for the large-deletion confirmation) |
| `tx:merge_clips(clips, {mode = "sequence" \| "stacked", track = t})` | Merge Clips in Sequence / Stacked with the Track List's rules (eligibility, subchannel mapping, muted clips refused); returns the merged clip |

A refusal is a sentence (`A clip needs a length of at least one tick.`) and the whole run
changes nothing. Reads inside the open transaction see its edits (the preview equals what
the commit applies).

## Notes

Needs `project.read`; note writes need **`notes.edit`**.

`t:notes{...}` (a track's own notes) and `c:notes{...}` (a clip content's) make a **query**:
a filter that is evaluated each time it is used, against the project as the script sees it
(inside a transaction: with its changes).

| Filter field | Selects notes whose |
|---|---|
| `start_tick`, `end_tick` | start is in `[start_tick, end_tick)` (default: all) |
| `keys` | key is `k`, or in `{low, high}` |
| `velocity` | velocity is `v`, or in `{low, high}` |
| `channel` | MIDI channel (0 – 15) is this one |
| `subchannel` | subchannel id is this one |
| `selected = true` | are selected |

| Read | Returns |
|---|---|
| `q:count()` | the exact number of matching notes (a start range alone is counted from chunk sizes, without visiting notes) |
| `q:batches(n)` | an iterator: `for b in q:batches(8192) do … end`; each `b` holds up to `n` notes (1 – 1,048,576; default 8,192) as arrays — `b.count`, `b.start`, `b.gate`, `b.key`, `b.velocity`, `b.channel`, `b.subchannel`, `b.selected`, `b.index` (the note's position in the track's stored order, 1-based). `q:batches{size = n, columns = {"start", "key"}}` builds only those arrays — every array costs time, so ask for what you read |
| `q:each()` | an iterator of note tables `{start, gate, key, velocity, channel, subchannel, selected, index}` |
| `q:handles([limit])` | note handles of the first `limit` (default 100,000) matching notes |

Notes come in stored order (by start; equal starts keep their order). An iteration reads
the notes as they were when it began (an immutable snapshot that shares PlumSpirit's
memory, no copy), so memory stays bounded however many notes the track has.

| Write (in a transaction) | Does |
|---|---|
| `tx:modify_notes(q, changes)` | changes every matching note **natively** (one pass in PlumSpirit, no Lua call per note); returns how many changed |
| `tx:modify_notes(q, function(n) … end)` | calls the function with each note table: return a table of new values (`start`, `gate`, `key`, `velocity`, `channel`, `subchannel`), `nil` to keep the note, `false` to delete it |
| `tx:delete_notes(q)` | deletes the matching notes; returns how many |
| `tx:duplicate_notes(q, changes)` | adds changed copies (e.g. `{transpose = 12}`) |
| `tx:insert_notes(t_or_c, notes)` | adds notes: a list of note tables, or arrays `{start = {...}, gate = {...}, key = {...}, velocity = {...}, channel = {...}, subchannel = {...}}`; `velocity` defaults to 100, `channel` to the track's default channel, `subchannel` to 0 |

`changes` (all optional, applied in this order): `shift` (ticks, may be negative),
`gate` (set) then `gate_scale` then `gate_add`, `transpose` (semitones), `velocity` (set)
then `velocity_scale` then `velocity_add` (the result always stays 1 – 127), `channel`,
`subchannel`, and `outside` — what happens to a note moved before tick 0 or outside the
project's keys: `"error"` (default: nothing is changed and the error names the note),
`"clamp"` or `"drop"`.

Every new or changed note is validated: key within the project's keys (0 – 127, or
0 – 255 in 256-Key Mode — never clamped silently), velocity 1 – 127, channel 0 – 15, a
subchannel the track has (add one first with `tx:add_subchannel`), no tick overflow. The
changed and inserted notes are selected afterwards, like after PlumSpirit's own tools.
Native loops check cancellation and the time budget every 65,536 notes; a native change
is many times faster than the function form, which costs one Lua call per note.

**Preview, Apply, Cancel.** A transaction is the non-destructive preview of its changes:
the script reads them, the project is untouched. `p:content_hash()` (a 64-bit hash of
everything the exported MIDI file depends on) read inside the transaction equals the
project's hash after the commit is applied (Preview = Apply); after `tx:cancel()` it is
the original hash again (Cancel leaves the project exactly as it was).

## Events (controllers, pitch bend, program changes)

Needs `project.read`; event writes need **`notes.edit`**.

`t:events{...}` / `c:events{...}` query a track's (or clip content's) preserved events.
Filter fields: `kind` (a name or a list), `channel`, `controller` (or `key`), `start_tick`,
`end_tick`. Kinds: `cc`, `pitch_bend`, `program`, `channel_pressure`, `poly_pressure`,
`note_on` / `note_off` (unpaired note messages kept from a file), and — read only, kept
byte for byte — `meta`, `sysex`, `sysex_escape`, `system`. **Tempo and time signatures are
not track events**: they belong to the Conductor (`p:tempos()`, `tx:set_tempo`); asking
for `kind = "tempo"` says so.

`q:count()`, `q:list([limit])` and `q:handles([limit])`. An event table has `tick`,
`index` (its position in the track's events, 1-based), `kind`, and by kind: `channel`
(0 – 15), `controller` + `value` (cc, 0 – 127), `value` (program / channel pressure,
0 – 127; pitch bend −8192 – +8191 with `raw` 0 – 16,383, 8,192 = center), `key` + `value`
(poly pressure), `key` + `velocity` (note messages), `meta_type` + `data` (meta: the
payload as a Lua string), `data` (SysEx), `status` / `data1` / `data2` (system).

| Write (in a transaction) | Does |
|---|---|
| `tx:insert_events(t_or_c, list)` | adds events (`{kind, tick, channel, controller or key, value}`; `channel` defaults to the track's default channel), each after the events already at its tick |
| `tx:modify_events(q, changes)` | `changes`: `shift` (ticks), `value` (set), `value_add` (clamped to the kind's range), `channel` — natively |
| `tx:modify_events(q, function(e) … end)` | return a table of new values, `nil` to keep, `false` to delete |
| `tx:delete_events(q)` | deletes the matching events |

Only `cc`, `pitch_bend`, `program`, `channel_pressure` and `poly_pressure` events can be
added, changed or deleted; a write whose query also matches other events (meta, SysEx,
note messages) is refused so they are never stripped by accident — add a `kind` filter.
Every value is checked against its range (never wrapped).

## Selection

Needs `project.read`.

| Call | Returns / does |
|---|---|
| `plum.selection.notes(t_or_c)` | a query of the selected notes (`t:notes{selected = true}`) |
| `plum.selection.analytics{window_seconds = 1}` | Selection Analytics of every selected note of the project (see below) |
| `plum.selection.active_track()` | the active track (the Piano Roll's), or `nil` |
| `plum.selection.tracks()` / `plum.selection.clips()` | the Track List's selected tracks / clips |
| `plum.selection.time()` | the ruler's time selection `{start_tick, end_tick}`, or `nil` |
| `plum.selection.set_tracks(tracks, {active = t})`, `set_active_track(t)`, `set_clips(clips)`, `set_time(start, end)` / `set_time()` (clear) | ask PlumSpirit to select them; reads in the same script see the request at once, and PlumSpirit applies it when the script ends successfully, so the highlight in the Track List and the ruler follows |
| `tx:select_notes(q, mode)` | sets the selected notes: `mode` = `"replace"` (default), `"add"`, `"remove"` or `"toggle"`; returns how many notes of that track are selected |
| `tx:clear_selection(t_or_c)` | deselects every note of the track |

Note selection is part of the project, so it changes in a transaction — but, as in the
Piano Roll, **without an Undo step**; the music does not change (`p:content_hash()` stays
the same). The app's selection (tracks, clips, time) is view state and needs no
transaction.

## Timing and analysis

Needs `project.read`.

| Call | Returns |
|---|---|
| `p:tempo_at(tick)` | `{bpm, bpm_float, micros, ramp, exact}` — exact decimal BPM at a tempo point or a step; inside a ramp the interpolated tempo with `exact = false` |
| `p:time_signature_at(tick)` | `{numerator, denominator, tick}` (`tick`: where it starts) |
| `p:bar_beat(tick)` | `{bar, beat, tick}` — 1-based bar and beat, the tick inside the beat (SMPTE timing has no bars: an error says so) |
| `p:tick_of(bar [, beat [, tick]])` | the tick of a bar / beat position, following every time signature change |
| `p:seconds_at(tick)` / `p:tick_at_seconds(seconds)` | through the tempo map (ramps included); `tick_at_seconds` gives the first tick at or after that time |
| `p:note_length(value)` | ticks of `"1/4"`, `"1/8t"` (triplet), `"1/16."` (dotted), `"1"`, `"2"`… exactly — an error when the value is not a whole number of ticks at the project's PPQN |
| `plum.timing.quantize(tick, grid [, mode])` | `"nearest"` (default; halves go up), `"floor"` or `"ceil"` |
| `p:metrics_at(tick, {window_seconds = 1})` | the Progressor's exact values at `tick`: `{notes_passed, total_notes, poly, nps, pps, seconds, exact = true}` |
| `q:analysis{window_seconds = 1}` | Selection Analytics of a note query: `{notes, counted, first_tick, last_tick, span_seconds, peak_poly, peak_poly_tick, peak_unique_keys, peak_nps, peak_nps_tick, peak_pps, avg_nps, avg_poly, window_seconds, exact = true}` — notes as they play (track corrections applied) |
| `q:estimate()` | `{notes, exact = false}`: a fast upper bound of the notes in the query's tick range from chunk sizes (no filter), for planning; `q:count()` is the exact count |
| `q:overlaps()` | `{notes, exact = true}`: notes that start while an earlier note of the same key and channel still sounds (stacked notes) |

The analysis results come from PlumSpirit's own engines — the same numbers as the
Selection Analytics readout and the Progressor; nothing is approximated without
`exact = false`.

## Transactions

Every change goes through a transaction: one change set, one Undo step, all or nothing.

```lua
local p = plum.project.current()
local tx = plum.transaction{ label = "Arrange" }   -- the label shown in Edit > Undo
local bass = tx:add_track{ after = p:track(1), name = "Bass" }
tx:add_subtracks(bass, 2)
tx:create_folder("Low end", { bass })
tx:set_tempo(1920, "143.5")
tx:commit()
```

* Each write is checked and applied **at once to a working copy**, so the script reads its
  own changes (`p:track_count()` already counts the new track; handles of deleted objects
  are expired). A refused write is a **runtime** error at the line that asked for it, with
  PlumSpirit's own sentence ("That change was refused: …").
* The project itself changes only after the script ends successfully: PlumSpirit applies
  each committed transaction as **one Undo step**. A script that fails, calls
  `tx:cancel()`, or ends with the transaction still open changes **nothing**.
* One transaction is open at a time; after `tx:commit()` a new one starts from the
  committed state (two commits = two Undo steps).
* `tx:estimate()` → `{operations, removed_notes, needs_confirmation}`. A transaction that
  removes **1,000,000 notes or more** (deleted tracks or subchannels) is applied only after
  the user confirms it.
* `tx:is_open()`, `tx:cancel()`, `tx:commit()`.

Track, folder and subchannel writes need **`tracks.edit`**; tempo, time signature and
marker writes need **`conductor.edit`** (and every write needs `project.read` to name its
objects). A list argument takes one handle or a table of handles (batches by handle).

| Write | Does |
|---|---|
| `tx:add_track{after = t, folder = f, name = s}` | a new track: after `t`'s subtree at its level (joining its folder), or at the end of folder `f`, or at the end of the list; returns its handle |
| `tx:add_subtracks(parent, n)` | `n` empty subtracks as `parent`'s last children; returns their handles |
| `tx:delete_tracks(tracks)` | deletes the tracks with their subtracks (never the last track) |
| `tx:move_tracks(tracks, target, place)` | moves the tracks' subtrees `"before"` / `"after"` / `"into"` `target` (order kept; no move into its own subtree) |
| `tx:rename(t_or_f, name)` | renames a track (its Track Name event, exported exactly once) or a folder |
| `tx:create_folder(name, tracks)` | a folder holding the top-level tracks of `tracks` (gathered after the first), or an empty one; returns its handle |
| `tx:delete_folder(f)` | removes the folder; its tracks stay |
| `tx:move_to_folder(tracks, f)` / `tx:remove_from_folder(tracks)` | folder membership of the tracks' top-level tracks |
| `tx:add_subchannel(t, {channel = c, name = s})` | a subchannel with the lowest free id (its MIDI channel: `c`, else the track's default channel); returns its handle |
| `tx:edit_subchannel(s, {channel = c, name = s, move_notes = b})` | changes its MIDI channel for new notes and its name; `move_notes = true` also moves its notes to the new channel |
| `tx:delete_subchannel(s)` | deletes the subchannel **and its notes** |
| `tx:set_tempo(tick, bpm, {ramp = b})` | adds or changes the tempo at `tick`; `bpm` is a decimal string (`"143.5"`, exact) or a number; `ramp = true` makes the tempo change linearly to the next point |
| `tx:remove_tempo(tick)` | removes the tempo change at `tick` (the one at tick 0 stays) |
| `tx:set_time_signature(tick, numerator, denominator)` | 1 – 255 beats of 1, 2, 4, … 128 |
| `tx:remove_time_signature(tick)` | |
| `tx:add_marker(tick, title [, end_tick])` | a marker, or a chapter to `end_tick` |
| `tx:remove_markers(tick)` | the markers starting at `tick` |

**Track List rows.** `p:rows()` lists what the Track List shows top to bottom — the
Conductor, folder headers (before each run of a folder's top-level tracks; an empty folder
before its anchor track or at the end) and tracks — as handles; `p:rows{visible = true}`
leaves out what collapsed tracks and folded folders hide.

**For the host (Rust).** After a run, `Host::take_commits()` returns the committed
transactions (`tx::Commits`); the app asks the user first when `needs_confirmation()`, then
calls `Commits::apply(history, &mut doc)`, which refuses when the project changed since the
run read it (`CommitError::ProjectChanged`). When a transaction needs more undo memory than
the project's history has (`CommitError::TooLargeForUndo`), the app asks "Apply without
undo?" as for any edit and calls `Commits::apply_without_undo`, which makes the project
exactly the transaction's result and clears the history. `Host::take_ui_selection()`
returns the selection the script asked for.

## Threads

* **Scripts run on a script worker** (`lua::worker::ScriptWorker`): a thread that owns
  its Lua state. PlumSpirit's UI thread hands it the script and a **snapshot** of the
  project (shared, immutable — no copy, no lock) and polls for the result without ever
  waiting; a progress bar reads what the script reports with `plum.progress`.
* A script **reads** only its snapshot (or its transaction's working copy) and never
  touches the project itself. What it commits comes back to the UI thread, which applies
  it as Undo steps (`Commits::apply`) — the only place a script's changes reach the
  project. While a script runs, the project is read-only to edits, as during PlumSpirit's
  other background jobs; playback and the views keep running.
* **Never in the audio callback**: no Lua runs on the audio or synthesizer threads, and
  scripts have no API that runs there.
* Cancel (the user's Cancel, or closing PlumSpirit) raises the worker's flag; the script
  stops at its next check and nothing is waited for. Dropping a worker cancels its script
  and lets the thread end on its own.
* One job at a time per worker; a defect that ends the worker thread is reported as an
  `internal` error and nothing is changed.

| Function | Use |
|---|---|
| `plum.progress(fraction [, message])` | reports progress (0 – 1, and a short message) to PlumSpirit; also a check point for cancellation |
| `plum.yield()` | a check point: a canceled script stops here (long loops in Lua are checked anyway every 10,000 instructions) |

## Plugins

A plugin is a folder with a manifest, its scripts and optional assets:

```text
graph-shift/
  plum-plugin.toml
  main.lua
  lib/curves.lua        (plum.plugin.include("lib/curves.lua"))
  presets/…             (assets)
```

**`plum-plugin.toml`** — one `key = value` per line (`#` comments; no tables):

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | stable id, 3 – 64 characters `a-z 0-9 . _ -` (`com.example.graph-shift`); the folder it is installed in |
| `name` | yes | shown in menus (1 – 80 characters) |
| `version` | yes | the plugin's version, numbers with dots (`1.2.0`) |
| `api` | yes | the lowest scripting API it needs (`"0.1"`): same major, equal or later minor runs it ([Versions](#versions)) |
| `entry` | no | the entry script, relative to the folder (default `main.lua`) |
| `capabilities` | no | what it asks to be allowed: `["project.read", "notes.edit"]` ([Capabilities](#capabilities)) |
| `description`, `author`, `license` | no | shown in the Lua Plugins window |
| `config_version` | no | the layout of its [settings](#settings-and-presets) (whole number, default 1) |

Unknown keys, a bad id or version, an entry outside the folder (`..`, absolute paths, drive
letters), reserved Windows names (`con`, `aux`, …), control characters and links inside the
package are refused with a sentence. Packages are at most 1,024 files, 16 MB per file and
128 MB in all.

**Installing** (Plugins → Lua Plugins…, or Settings → Lua Plugins): from a folder (with
`plum-plugin.toml`) or a single `.lua` file (its id becomes `local.<file name>`; a
`-- plum-capabilities: project.read, notes.edit` comment in its first 20 lines says what it
asks for). The package is copied into the plugins folder (`%APPDATA%\PlumSpirit\plugins\`
unless Settings → Lua Plugins → Plugins Folder names another) through a staging folder that
is renamed into place only when every file was copied and read back — a failed install or
update leaves the plugins folder as it was. **Nothing is allowed or enabled by installing**:
the window lists what the plugin asks for, the user ticks each capability and enables it
(Settings → Lua Plugins → Enable Plugins When Installed changes that default). Update
replaces a plugin with a package of the same id (capabilities the new version no longer asks
for are withdrawn; new ones wait for the user). Remove deletes its folder and forgets its
settings. Refresh re-reads the folder and reloads plugins whose files changed. PlumSpirit
never downloads or installs anything by itself. Each plugin is isolated: it cannot load
another plugin's files or share its Lua state.

**Lifecycle.** Each enabled plugin runs on its own [script worker](#threads), in its own Lua
state with only the capabilities the user allowed:

1. the entry script runs (top-level code; registrations allowed);
2. `on_load()`, then `register(plum.plugin)` — the place to register commands;
3. `on_project_open(project)` when a project is (or already was) open — `project` is `nil`
   without `project.read`; `on_project_close()` when it closes;
4. commands run when the user chooses them (Plugins → the plugin's name → the command);
5. `on_unload()` when the plugin is disabled, removed, reloaded or PlumSpirit closes (at most
   3 s; then the state is dropped anyway).

Every hook is optional. Lifecycle hooks may only read: a transaction committed in a hook is
not applied (the log says so). A syntax error, an error in a hook or `register()`, or a
newer `api` than PlumSpirit has makes the plugin **Failed** / **Needs a newer
PlumSpirit** with the reason (file and line) in its log; nothing reaches the open project
and PlumSpirit keeps running. A reload is a fresh Lua state: registrations are never
doubled.

**`plum.plugin`** (plugins only)

| Call | Does |
|---|---|
| `plum.plugin.id` / `.name` / `.version` | the manifest's values |
| `plum.plugin.command{id =, name =, tooltip =, run = function(ctx) … end}` | registers a command (while loading only; an id is 1 – 64 characters `a-z 0-9 . _ -`, unique in the plugin). `ctx.command` is its id. What `run` commits in transactions is applied through the undo history (one step per transaction); a returned text is shown in the status bar |
| `plum.plugin.include(path)` | runs a `.lua` file of the plugin's own folder and returns its results (paths are checked like the manifest's) |

A command runs on the open project's snapshot; while it runs the user keeps working, and if
the project changed meanwhile its changes are not applied (`ProjectChanged`). One command
runs at a time per plugin; lifecycle hooks in progress finish first. The Lua Plugins window
shows a running command's progress (`plum.progress`) with a Cancel button.

**Log.** Every plugin has a log (the last 1,000 lines, UTC times): loading, what it printed,
every error with its file and line, warnings (a hook's changes not applied, a slow
`on_unload`). **Copy for a Bug Report** copies it with the user's home folder
(`%USERPROFILE%`), user name (`<user>`) and other folders (`<path>\file.lua`) removed.

**Settings → Lua Plugins** (`settings.json` → `lua_plugins`): Run Lua Plugins (off: none
runs; everything installed and allowed is kept), Enable Plugins When Installed (default
off), Plugins Folder, and per plugin enabled / allowed capabilities / origin.

## UI integration

Plugins never draw their own windows: PlumSpirit renders everything from declarations, so
every plugin looks and behaves like the rest of the program (theme, keyboard, American
English for built-in text).

| Where | How |
|---|---|
| Plugins → *plugin name* → command | `plum.plugin.command{…}`; a command with `params` shows **…** after its name and opens an options dialog first |
| Tools → Custom → *tool*, note context menu → Custom | `plum.plugin.tool{…}` ([Custom tools](#custom-tools)) |
| status bar | `plum.ui.notify(text, kind)` and a command's returned text |
| running indicator | above the status bar while a command runs longer than 0.3 s: the plugin, the command, `plum.progress` (fraction and message) and **Cancel** |
| Lua Plugins window | status, log, progress and Cancel of every plugin |
| graph editor | a `curve` option or tool parameter is edited in PlumSpirit's curve editor (the one Event Graphs and the built-in tools use) and reaches the script as `{points, at(x)}`, evaluated exactly like built-in curves |
| Preview | custom tools preview live in the Piano Roll with the same run that Apply commits (Preview = Apply) |

**Command options.** `params` takes the [custom tools' parameter kinds](#custom-tools)
(int, float, bool, choice, pitch, ticks, seed, color, curve, range, subchannel map, note
filter, text). The dialog shows one native control per option with its label, help (on
hover) and a reset button; **Run** (or Enter) runs the command with `ctx.params`, **Cancel**
(or Escape) runs nothing. The last values are offered again while PlumSpirit runs.

```lua
p.command{ id = "level", name = "Set Level", tooltip = "Sets every velocity.",
  params = { { key = "amount", label = "Velocity", kind = "int", min = 1, max = 127, default = 40 } },
  run = function(ctx)
    local tx = plum.transaction{ label = "Set Level" }
    tx:modify_notes(plum.project.current():track(1):notes(), { velocity = ctx.params.amount })
    tx:commit()
    plum.ui.notify("Velocities set to " .. ctx.params.amount .. ".")
  end }
```

**`plum.ui.notify(text, kind)`** — `kind` is `"info"` (default), `"warning"` or `"error"`.
Shown in the status bar after the run, prefixed with the plugin's name; control characters
are removed, at most 300 characters and 5 notifications per run (more are dropped, blank
ones ignored). It never interrupts the user (no modal dialogs).

## Settings and presets

Every plugin has its own settings, kept apart from every other plugin's; values are nil,
booleans, numbers (finite), texts and tables of them (arrays or text keys, at most 16
levels, 1 MB per scope). Keys and preset names are 1 – 64 characters `A-Z a-z 0-9 _ - . `
and spaces. A value that cannot be stored is refused with a sentence and nothing changes.

| Call | Does |
|---|---|
| `plum.config.get(key, default)` / `set(key, value)` / `remove(key)` / `keys()` | the **user** settings: `%APPDATA%\PlumSpirit\plugin-data\<id>\config.json`, written atomically (temporary file, then rename) when a run that changed them ends successfully; `set(key, nil)` removes |
| `plum.config.project.get` / `set` / `remove` / `keys` | the **project** settings: stored in the open project (`.lup` section `PLUG`), outside the undo history; a change marks the project changed, like Mute / Solo. Needs an open project |
| `plum.config.version` | the manifest's `config_version` |
| `plum.presets.save(name, value)` / `load(name)` (nil when missing) / `list()` (sorted) / `delete(name)` | named presets in `plugin-data\<id>\presets\<name>.json` (at most 1,000) |

A run that fails saves nothing: its changes to either scope are dropped. Without a settings
folder (`PLUMSPIRIT_CONFIG_DIR` unusable) user settings last for the session and presets are
unavailable.

**Versions and migration.** Stored data is `{"version": n, "values": {…}}`. When the
manifest's `config_version` is higher than the stored one, PlumSpirit calls the plugin's
global `migrate_config(old_version, values)` (when it has one) and stores what it returns
with the new version; without one the values are kept as they are. Data written by a newer
version of the plugin is **kept but not used** (nothing is overwritten). A damaged
`config.json` is set aside as `config.json.damaged-<time>` and the plugin starts with empty
settings; damaged project settings are ignored until the plugin saves new ones. Each case is
written to the plugin's log.

```lua
function migrate_config(old, v)
  if old < 2 then v.speed = (v.speed or 1) * 100 end   -- layout 2 stores percent
  return v
end
```

## Custom tools

A plugin's note tools appear under **Tools → Custom** (grouped by plugin) and in the Piano
Roll's right-click menu under **Custom**; the built-in Tools, Experimental, Generator,
Iterations and Legacy sections are unchanged. Register them while loading (like commands):

```lua
function register(p)
  p.tool{
    id = "graph-shift",                -- stable within the plugin (a-z 0-9 . _ -)
    name = "Graph Shift",              -- menu entry and the Undo step's name
    tooltip = "Shifts each selected note by a graph over the selection.",
    needs_selection = true,            -- default true; false: all notes when none are selected
    params = {
      { key = "amount", label = "Amount", kind = "ticks", min = 0, max = 3840, default = 240,
        help = "The largest shift." },
      { key = "shape", label = "Shape", kind = "curve", default = { { 0, 0 }, { 1, 1 } } },
      { key = "seed", kind = "seed" },
    },
    run = function(ctx)
      -- ctx.tx is open; PlumSpirit commits it after run returns.
      local n, i = ctx.notes:count(), 0
      ctx.tx:modify_notes(ctx.notes, function(note)
        i = i + 1
        local x = n > 1 and (i - 1) / (n - 1) or 0
        return { start = note.start + math.floor(ctx.params.amount * ctx.params.shape.at(x) + 0.5) }
      end)
    end,
  }
end
```

**Parameter kinds** (each becomes a native control in the tool's window; the window checks
every value against the declaration, and so does the host before `run` sees it):

| `kind` | Fields | Control | `ctx.params.key` |
|---|---|---|---|
| `int` | `min`, `max`, `default` | slider | integer |
| `float` | `min`, `max`, `unit`, `default` | slider | number |
| `bool` | `default` | checkbox | boolean |
| `choice` | `choices = {…}`, `default` (a choice) | drop-down | the chosen text |
| `pitch` | `default` | slider with note names, up to the project's top key (127 or 255) | integer |
| `ticks` | `min`, `max`, `default` | number field | integer ticks |
| `seed` | `default` | number + dice | integer (also `ctx.seed`) |
| `color` | `default = {r =, g =, b =}` | color button | `{r, g, b}` |
| `curve` | `default = {{x, y}, …}` (0 – 1, 2 – 64 points) | the graph editor of the built-in tools | `{points = …, at = function(x)}` — `at` evaluates the graph exactly as built-in tools do |
| `range` | `min`, `max`, `default = {lo, hi}` | two number fields | `{lo, hi}` with `.lo`, `.hi` |
| `subchannel_map` | `default = {{from, to}, …}` | rows with Add / × | `{{from =, to =}, …}` |
| `note_filter` | `default = {keys = {lo, hi}, velocity = {lo, hi}, channel = n}` | keys, velocities, channel | the same table — pass it to `track:notes(...)` |
| `text` | `max_len`, `default` | text field | string |

Every parameter has a `key` (`a-z 0-9 _`, unique), an optional `label` and `help` (its
tooltip). At most 64 parameters.

**`ctx`**: `params`; `track` — the track (or the clip content) the Piano Roll edits;
`notes` — a query of its selected notes (or, for `needs_selection = false` without a
selection, all its notes); `selection` (true when notes were selected); `tx` — the open
transaction labelled with the tool's name; `preview` (true while the window previews);
`seed`; `keys` (128 or 256); `tool` (its id).

**Preview / Apply / Cancel.** The window runs the tool on a snapshot of the project each time
its inputs change (controls, the edited notes, their selection) and draws the result notes in
the Piano Roll; the project does not change. **Apply** applies exactly the previewed
transaction through the undo history — one Undo step — or, when the project changed since the
preview, runs the tool once more and applies that. **Cancel** closes the window; nothing was
changed. Results above 2,000,000 notes are not drawn (Apply still works). Every editor rule
holds because a tool can only change the project through the API's transactions: exact ticks
and gates, 128 / 256 keys, the Conductor's global tempo, track / subtrack / clip contents,
subchannels, Selection Opacity in the preview, Undo / Redo.

**Presets** (`%APPDATA%\PlumSpirit\presets\custom\<plugin>\<tool>\<name>.json`): Save, Load,
Delete in the window; a value the declaration no longer accepts keeps its default and says
so. **Shortcut**: a key that opens the tool (Shortcut in the window); a chord PlumSpirit or
another custom tool uses is refused. Saved in `settings.json` → `lua_plugins.tool_keys`.

**Editing a plugin while PlumSpirit runs**: change its files in the plugins folder, then
**Refresh** in the Lua Plugins window. A plugin whose files changed is loaded again in a new
state while the old one keeps working; the new one replaces it when it is ready, and if it
fails (a syntax error, an error in `register()`) the old one stays and the reason is in the
log (**rollback**). Registrations are rebuilt, never doubled.

## Examples

PlumSpirit ships two example plugins inside the program (their files are compiled in, so no
download is needed): **Plugins → Lua Plugins… → Examples → Install Graph Shift / Install
CC Batch**. Installing copies the example into the plugins folder like any other package
(origin *Example* in the window); as for every plugin, nothing is allowed or enabled until
you tick its capabilities (`project.read`, `notes.edit`) and enable it. **Reinstall**
restores the original files after you experimented with them (your decisions are kept).
The sources are also in the repository: `crates/plumspirit/examples-lua/`.

| Example | Where | What it shows |
|---|---|---|
| **Graph Shift** (`plumspirit.example.graph-shift`) | Tools → Custom → Graph Shift, and the note menu → Custom | a [custom tool](#custom-tools) with an `int`, a `curve` (the graph editor) and a `choice` parameter; streaming a query with `q:batches{columns = {"start"}}`; `ctx.tx:modify_notes` with a function (keep / change / delete); live Preview = Apply, one Undo step; deterministic (same notes and settings → the same result) |
| **CC Batch** (`plumspirit.example.cc-batch`) | Plugins → CC Batch (Example) → Write CC Over Selection… / Clear CC Over Selection… | two [commands with options dialogs](#ui-integration) (controller, Graph or LFO shape, cycles, value range, step, replace); `plum.selection.notes` / `active_track` / `time`; `t:events{…}`, `tx:delete_events`, `tx:insert_events` in one [transaction](#transactions) (one Undo step); `plum.progress`; `plum.ui.notify`; the events are saved in the `.lup` project and exported to MIDI |

Graph Shift: each selected note moves by `round(Shift × Shape(x))` semitones, `x` running
from 0 at the first selected start to 1 at the last; At the Keyboard's Edge says whether a
note moved past key 0 or the top key is clamped, kept where it is, or removed. CC Batch
writes one event every Step ticks from the first selected start to the last selected end (or
across the ruler's time selection when no notes are selected), on the first selected note's
channel (or the track's default channel), leaving out repeated values; with Replace Existing
it first removes that controller's events on that channel inside the span, so running it
twice never doubles events. Both are covered by `lua::example_tests`.

## Threat model

Community scripts are **untrusted**. What the sandbox gives:

* no file system, process, network, native-module or debug access unless a capability is
  granted, and file capabilities only reach files the user picks in a dialog;
* bounded memory, bounded time between checks, cancellation;
* no binary chunks (which could break the interpreter's memory safety).

What it does not give: the script runs **in PlumSpirit's process**. A defect in the Lua
interpreter or in a PlumSpirit host function could be exploited by a hostile script, and a
script can still consume CPU until its next check or a long library call ends. Process
isolation was evaluated: a separate process would have to receive the notes it works on
(millions for Black MIDI — copying them costs seconds and gigabytes on a 2 GB PC) and is
not used in beta1. Install scripts you trust; PlumSpirit shows a plugin's origin and the
capabilities it asks for before enabling it, and never installs from the network.

## Reference index

Every call of API 0.1.0, by family. `p` = project, `t` = track / subtrack, `c` = clip,
`q` = a note or event query, `tx` = a transaction, `s` = subchannel, `f` = folder.

| Family | Calls | Section |
|---|---|---|
| Versions | `plum.version`, `plum.api_version`, `plum.api.major / minor / patch`, `plum.require_api(major [, minor])`, `plum.lua_version` | [Versions](#versions) |
| Capabilities | `plum.capabilities()`, `plum.has(name)` | [Capabilities](#capabilities) |
| Exact ticks | `plum.ticks.check`, `parse`, `add`, `sub`, `mul`, `muldiv(a, num, den [, mode])`, `plum.ticks.max`, `plum.ticks.exact_float_max`, `plum.max_integer` | [Numbers](#numbers) |
| Every handle | `h:kind()`, `h:key()`, `h:valid()`, `h:check()`, `==`, `tostring(h)` | [Handles](#handles) |
| Project | `plum.project.current()`, `p:name()`, `file()`, `file_type()`, `dirty()`, `revision()`, `format()`, `ppqn()`, `division()`, `key_range()`, `duration_ticks()`, `duration_seconds()`, `track_count()`, `note_count()`, `selected_note_count()`, `content_hash()`, `tracks()`, `track(n)`, `folders()`, `rows{visible =}`, `conductor()`, `clips{…}`, `clips_at(tick)` | [Project](#project), [Clips](#clips) |
| Conductor | `p:tempos()` / `c:tempos()`, `time_signatures()`, `markers()` | [Project](#project) |
| Timing | `p:tempo_at(tick)`, `time_signature_at(tick)`, `bar_beat(tick)`, `tick_of(bar [, beat [, tick]])`, `seconds_at(tick)`, `tick_at_seconds(s)`, `note_length(value)`, `metrics_at(tick, {…})`, `plum.timing.quantize(tick, grid [, mode])` | [Timing and analysis](#timing-and-analysis) |
| Tracks | `t:name()`, `label()`, `id()`, `index()`, `depth()`, `parent()`, `children()`, `folder()`, `color()`, `mute()`, `solo()`, `collapsed()`, `visible()`, `note_count()`, `stored_note_count()`, `clip_count()`, `default_channel()`, `subchannels()`, `notes{…}`, `events{…}`, `clips{…}`, `clips_at(tick)` | [Tracks](#tracks-subtracks-and-folders) |
| Subchannels / folders | `s:id()`, `number()`, `name()`, `channel()`, `track()`; `f:id()`, `name()`, `tracks()`, `collapsed()` | [Tracks](#tracks-subtracks-and-folders) |
| Clips | `c:track()`, `start_tick()`, `end_tick()`, `song_length()`, `offset()`, `window()`, `length()`, `content()`, `content_kind()`, `is_primary()`, `linked_count()`, `linked_clips()`, `to_song(local)`, `to_local(song)`, `contains(tick)`, `mute()`, `name()`, `color()`, `notes{…}`, `events{…}` | [Clips](#clips) |
| Note queries | `q:count()`, `q:batches(n \| {size =, columns =})`, `q:each()`, `q:handles([limit])`, `q:analysis{…}`, `q:estimate()`, `q:overlaps()` | [Notes](#notes), [Timing and analysis](#timing-and-analysis) |
| Event queries | `q:count()`, `q:list([limit])`, `q:handles([limit])` | [Events](#events-controllers-pitch-bend-program-changes) |
| Selection | `plum.selection.notes(t_or_c)`, `analytics{…}`, `active_track()`, `tracks()`, `clips()`, `time()`, `set_tracks`, `set_active_track`, `set_clips`, `set_time` | [Selection](#selection) |
| Transactions | `plum.transaction{label =}`, `tx:commit()`, `tx:cancel()`, `tx:is_open()`, `tx:estimate()` | [Transactions](#transactions) |
| Track / folder / subchannel writes | `tx:add_track{…}`, `add_subtracks`, `delete_tracks`, `move_tracks`, `rename`, `create_folder`, `delete_folder`, `move_to_folder`, `remove_from_folder`, `add_subchannel`, `edit_subchannel`, `delete_subchannel` | [Transactions](#transactions) |
| Conductor writes | `tx:set_tempo`, `remove_tempo`, `set_time_signature`, `remove_time_signature`, `add_marker`, `remove_markers` | [Transactions](#transactions) |
| Note writes | `tx:modify_notes(q, changes \| function)`, `delete_notes`, `duplicate_notes`, `insert_notes`, `select_notes(q, mode)`, `clear_selection` | [Notes](#notes), [Selection](#selection) |
| Event writes | `tx:insert_events`, `modify_events(q, changes \| function)`, `delete_events` | [Events](#events-controllers-pitch-bend-program-changes) |
| Clip writes | `tx:add_clip`, `move_clips`, `duplicate_clips`, `resize_clip`, `slice_clips`, `set_muted`, `rename_clip`, `recolor_clips`, `delete_clips`, `merge_clips` | [Clips](#clips) |
| Threads | `plum.progress(fraction [, message])`, `plum.yield()` | [Threads](#threads) |
| Plugins | `plum.plugin.id / name / version`, `plum.plugin.command{…}`, `plum.plugin.tool{…}`, `plum.plugin.include(path)`; hooks `on_load`, `register`, `on_project_open`, `on_project_close`, `on_unload`, `migrate_config` | [Plugins](#plugins), [Custom tools](#custom-tools) |
| UI | `plum.ui.notify(text, kind)`, command `params` (options dialog), `print` (the plugin's log) | [UI integration](#ui-integration) |
| Settings / presets | `plum.config.get / set / remove / keys`, `plum.config.project.*`, `plum.config.version`, `plum.presets.save / load / list / delete` | [Settings and presets](#settings-and-presets) |
| Custom tool `ctx` | `params`, `track`, `notes`, `selection`, `tx`, `preview`, `seed`, `keys`, `tool` | [Custom tools](#custom-tools) |

**Where things live** (Windows; `PLUMSPIRIT_CONFIG_DIR` replaces `%APPDATA%\PlumSpirit`):
plugins `%APPDATA%\PlumSpirit\plugins\<id>\` (or Settings → Lua Plugins → Plugins Folder),
plugin settings and presets `%APPDATA%\PlumSpirit\plugin-data\<id>\`, custom tool presets
`%APPDATA%\PlumSpirit\presets\custom\<plugin>\<tool>\`, the switches in `settings.json` →
`lua_plugins`. Custom tools: **Tools → Custom → *plugin* → *tool*** and the Piano Roll's
note menu → **Custom**; commands: **Plugins → *plugin* → *command***.

## Compatibility checklist

Before sharing a plugin, check:

1. **Manifest**: `id` is unique and stable (reverse domain, `a-z 0-9 . _ -`), `version`
   goes up with every release, `api = "0.1"` (the lowest API it needs), `capabilities`
   lists only what it uses — users see the list before enabling it.
2. **Version check**: the entry script starts with `plum.require_api(0, 1)`; while the API's
   major is 0, re-test on every new PlumSpirit beta (CHANGELOG.md lists API changes).
3. **No outside world**: only the sandbox's libraries (no `io`, `os`, `require`, `debug`,
   binary chunks); other files of the plugin through `plum.plugin.include`.
4. **Exact numbers**: ticks, counts, keys and values are integers (`math.type(x) ==
   "integer"`); compute large ticks with `plum.ticks` (Lua integers wrap silently); never
   pass a float above 2⁵³ as a tick.
5. **Keys and channels**: keys 0 – 127, or 0 – 255 when `p:key_range()` (or `ctx.keys`) is
   256; channels 0 – 15 in the API (shown 1 – 16); velocity 1 – 127; subchannels are not
   MIDI channels.
6. **Tempo is global**: change tempo / meter / markers only through the Conductor writes
   (`conductor.edit`); there is no per-track tempo.
7. **Every change in a transaction**, one Undo step each; a failed or canceled run changes
   nothing — don't try to undo your own changes by hand.
8. **Millions of notes**: stream with `q:batches{columns = …}` (ask only for the columns you
   read), prefer the native `changes` tables over a function per note, call
   `plum.progress` in long loops, and stay inside the output budget.
9. **Handles, not indexes**: keep handles (or `h:key()`), not list positions; read notes
   again after a commit (note and event handles are snapshots).
10. **Determinism**: a tool's result should depend only on the notes and its parameters —
    use `ctx.seed` for randomness, so Preview = Apply and presets give repeatable results.
11. **Lifecycle hooks only read**; register commands and tools in `register(p)`; settings
    through `plum.config` with a `config_version` and `migrate_config` when their layout
    changes.
12. **Readable messages**: user-facing text in full sentences (`error("Choose a Step of at
    least one tick.")`, `plum.ui.notify(…)`); PlumSpirit adds the file, line and category.
13. **Test it** on 1 note, no selection, the keyboard's edges, a subtrack, a clip, an empty
    project, Undo / Redo, and Cancel during a long run.

## Reporting a problem

Every error a user sees names its **category** ([Errors](#errors)), the plugin's file and
line, and PlumSpirit's sentence. The plugin's log (Lua Plugins window → the plugin) keeps
the last 1,000 lines; **Copy for a Bug Report** copies it with the user's home folder, user
name and other folders removed, ready to paste into a report for the plugin's author. A
`internal` error is a PlumSpirit defect: report it to PlumSpirit with that copy; nothing in
the project was changed.
