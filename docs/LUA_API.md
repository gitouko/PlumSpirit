# PlumSpirit Lua scripting — API 0.2.0

PlumSpirit 1.0.1 supports API 0.2.0 and existing API 0.1 plugins. The new services
include selected files, bounded images and masks, streamed notes, Group and metadata
operations, reactive observations, declared plugin services, and native geometry.
Plugin authors can compose these services with fonts, declarative panels, graphs,
Piano Roll drawing, overlays, transport, and custom formats. The 22 examples contain
their own authored Lua algorithms.

The font sections list the supported glyph and shaping paths. MIDI input requires
explicit device approval. The Windows test machine reported zero input devices.
Its typed input and lifecycle tests use manufactured packets, and the audio output
was exercised on the actual Windows device. The requirement and verification records
are in [v1.0.1 development records](v1_0_1_api02/REQ_GRAPH.toml).

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
| `plum.version` | PlumSpirit's version, `"1.0.1"` in this release |
| `plum.api_version` | the scripting API's version, `"major.minor.patch"` (now `"0.2.0"`) |
| `plum.api.major` / `.minor` / `.patch` | the same as integers |
| `plum.require_api(major [, minor])` | `true` when this PlumSpirit runs a script written for `major.minor`, else a **compatibility** error saying which version the script needs |

Semantic versioning: a **patch** changes nothing scripts can see; a **minor** only adds
(new functions, fields, capabilities); a **major** may change or remove things. A script
written for `major.minor` runs on every host with the same major and an equal or later
minor. During API major version 0, changes are recorded in CHANGELOG.md. For this upgrade, existing 0.1 plugins remain compatible: keep `plum.require_api(0, 1)` and `api = "0.1"` when using only that surface. New selected-file calls require `plum.require_api(0, 2)` and `api = "0.2"`. No old grant is expanded into arbitrary filesystem authority; each operation requires fresh user selection.

### Moving an existing 0.1 plugin to 0.2

An unchanged 0.1 package needs no migration. Its native `curve` parameter, tool
registration, note rows, default packed columns, presets and transactions remain
available. API 0.2 custom canvases and drawing tools coexist with the native editors.

When adopting new calls, declare `api = "0.2"` in `plum-plugin.toml` and call
`plum.require_api(0, 2)` before using them. Add only the capabilities those operations
need. Updating or installing a package does not grant new permissions. Separate
interactive commands from load hooks, observers and Preview: those contexts cannot
open file/device consent dialogs or perform unrestricted UI/audio work.

Keep versioned plain input data in config/presets/project metadata. File tokens,
fonts, images, stroke handles, transactions and UI resources have documented scopes;
do not serialize them or retain them for a later job. Preserve exact tick integers
(or documented decimal tick strings) instead of converting large positions to floats.
Use the native transaction/writer and Preview/Apply path for musical edits; private
staging is published atomically through the existing Undo system. Authors supply
their creative algorithms; adopting API 0.2 does not generate advanced functions.

Diagnostic output now has explicit byte and line limits (below). Keep large data in
bounded native resources or an explicitly selected output file rather than printing
large dumps to the plugin log.

## Capabilities

Outside the sandbox a plugin can only do what the user granted it, per plugin (the
plugin's settings; the manifest asks for them). Names are stable:

| Capability | Allows |
|---|---|
| `midi.input` | discover MIDI input names and read bounded short messages from a device explicitly approved for each command; Windows only; separate from project/playback editing |
| `playback.control` | bounded conflict-checked transport requests from explicit commands/non-preview tools; also needs `playback.read` |
| `playback.emit_midi` | explicit bounded staged MIDI on isolated internal playback voices, own-stream cancellation; also needs `playback.read`; no project edits/shared external routes |
| `playback.read` | captured transport snapshots and bounded `transport_changed` worker observations; no playback control or note access |
| `project.read` | reading tracks, clips, notes, events, the Conductor and the selection |
| `project.observe` | bounded worker-delivered project metadata notifications; also needs `project.read` |
| `project.automate` | bounded automatic note transactions requested by an observer; also needs `project.read`, `project.observe` and `notes.edit` |
| `services.provide` | publish bounded plain-data provider services on the plugin worker |
| `services.call` | call explicitly declared services of enabled plugins through the bounded host broker |
| `metadata.edit` | change this plugin's typed project attributes through native transactions and Undo |
| `fonts.list` | enumerate installed family/face metadata and load scoped installed fonts; no arbitrary file access |
| `ui.panels` | user-invoked declarative plugin panels/canvas and bounded typed input events |
| `ui.overlay` | bounded native editor overlay frames, retained UI pixels and surface notifications; Piano Roll also needs `project.read`, Player also needs `playback.read`; separate from panels |
| `notes.edit` | changing notes and controller events (through Preview / Undo) |
| `tracks.edit` | adding, removing and changing tracks, subtracks, folders and clips |
| `conductor.edit` | changing tempo, meter and markers |
| `file.read_user_choice` | reading a file the user picks in a dialog |
| `file.write_user_choice` | writing a file the user picks in a dialog |

`plum.capabilities()` returns every name with `true` / `false` (granted or not);
`plum.has(name)` answers one. An API function needing a capability that was not granted
fails with a **capability** error naming it — before doing anything.

## Private package assets (API 0.2)

`plum.plugin.asset(relative_path)` opens a read-only file **inside the invoking plugin's
installed package**. Installation already supplies these private assets; no user-file
permission or picker is required. It grants no access to arbitrary paths, another
plugin, the plugin-data folder, asset directory enumeration, writes or executable code.
Only plugins have this call. Use `plum.files.open()` for files outside the package;
that still requires user selection and `file.read_user_choice`.

The relative UTF-8 path is at most1024bytes and follows the existing package rules:
at most8safe components,120bytes/component, no root/drive/UNC/device path, `.`/`..`,
empty component, alternate stream, reserved Windows name, trailing dot/space or control
character. Both `/` and `\` separate components. Missing/unsafe/unreadable assets are
errors, not a canceled picker. Reparse points/symlinks/junctions and hard-linked files
are refused. Existing native directory pins and read-only open-handle sharing prevent
path replacement, renaming or writes while the asset is open on Windows.

At most `plum.plugin.max_asset_opens` (32) opens may be attempted per job, including
closed and failed attempts. A file is at most `plum.plugin.max_asset_bytes` (16777216,
16MiB), matching the existing package single-file limit. Each open reserves256KiB of
native metadata budget before opening handles; that reservation remains with retained
userdata until GC. The existing shared1GiB/job cumulative read budget,1MiB/read chunk
and8MiB/read_all cap apply. The asset exposes the same `info/read/read_all/seek/close`
methods as selected input files. `write` and `commit` refuse it. `info().source` is
`"package_asset"`; selected-file info uses `"user_choice"`. No absolute path is returned.

Assets share the current job lease and plugin ID/root authority. Job end, failure,
cancellation, reload or disable closes native file/directory handles eagerly; an old
userdata cannot read a later job. Reopen the relative asset name in each invocation.
Images and fonts consume the existing authorized stream, restore its cursor after
loading, and keep their own job-scoped decoded/copied resources. No decoder is duplicated.
Font info reports `source="package_asset"`; bounded metrics and supported original
glyph outlines work. On Windows, package fonts use the same resource-isolated external
shaping route described under `shape_run`; loading does not give them bundled-font trust.

For a package containing `assets/guide.png` and a properly licensed
`assets/face.ttf`, the plugin author can write:

```lua
local guide_file=plum.plugin.asset("assets/guide.png")
local guide=plum.images.load(guide_file)
guide_file:close()
local face_file=plum.plugin.asset("assets/face.ttf")
local face=plum.text.load_font(face_file)
face_file:close()
local outline=face:glyph_outline(assert(face:glyph_id("O")))
-- Author maps outline or guide pixels into graph/canvas/brush/note primitives.
-- Resources stay scoped to this invocation; store relative names in settings.
face:close()
guide:close()
```

Distribute font/image license files with the plugin and verify redistribution rights.
Existing API0.1 `plum.plugin.include` and selected-file grants keep their behavior.

## Transport snapshots and observations (API 0.2)

`plum.playback.snapshot()` requires the separately granted `playback.read` capability.
It returns a fresh plain table from native facts captured with the command/tool/lifecycle
run, or `nil` when that run has no native transport context. Repeated calls within a run
read the same captured values. Local table changes do not modify native transport.

| Field | Meaning |
|---|---|
| `schema`, `project`, `revision` | schema1 and exact native project/revision integers |
| `action_revision` | exact native transport-action generation used by the host to reject stale control requests |
| `state`, `running` | `stopped`, `playing`, `paused`, `seeking` or `audio_unavailable`; seeking/playing count as running |
| `tick`, `anchor` | exact song tick; optional native edit/anchor tick when anchor policy is enabled |
| `loop` | optional `{start,finish}` native enabled loop, half-open ticks |
| `configured_loop` | persisted native setting `{enabled=boolean,start=?,finish=?}`, including disabled ranges |
| `speed`, `bpm` | native audition speed multiplier and Conductor tempo at the captured tick, including ramps |
| `meter` | `{numerator,denominator,clocks,thirty_seconds}` from the authoritative Conductor |
| `division` | `{kind="ppqn",ppqn=...}` or `{kind="smpte",frames_per_second=...,ticks_per_frame=...}`;29 denotes native29.97drop-frame timing |

Ticks follow the existing integer API: values above2^63−1 fail explicitly, never round.
The snapshot omits paths, note payloads and device identifiers. It uses binary-search
Conductor queries and native transport getters, with no note traversal or tempo-map build.

Plugins can register the new typed event through the existing bounded observer API:

```lua
plum.require_api(0, 2)
plum.plugin.observe {
  id="transport", events={"transport_changed"}, debounce_ms=100,
  run=function(event)
    local s=plum.playback.snapshot() -- same captured native data as event.data
    print(s.state, s.tick, s.bpm, s.speed)
  end
}
```

A transport-only subscription requires only `playback.read`. The surface_changed UI
event uses ui.overlay plus its surface read authority as documented below. Other project event names
still require `project.read` plus `project.observe`; mixed subscriptions need both sets.
`event.data` has the snapshot schema above; the usual sequence/coalesced/dropped, project,
revision, `context="metadata"` and `read_only=true` envelope applies. Transport observations
always have `transaction_allowed=false`. They cannot trigger automatic note transactions.

Sampling runs at most20Hz only while a transport subscriber is enabled and a project
is open. It reuses the32subscription/64pending-entry queue, maximum8worker starts/second
and250ms per delivery. New positions coalesce to the latest pending state without
canceling an already running same-revision transport handler. Native project revisions,
close, disable, reload and permission revocation still invalidate stale jobs.
These are state observations; intermediate positions/transitions can coalesce.
They are unsuitable as a sample-accurate audio clock or complete event log. No Lua runs
on rendering/audio callbacks. Transport-only subscriptions do not activate project-store
sampling. An occupied plugin worker handles pending observations after its current job.

**Transport Monitor** is an installable, embedded example with only `playback.read`.
Its explicit command saves the run snapshot and last observed state to its own preset.
Transport control, loop settings, and precomputed MIDI plans use their own calls.
Control and output require their separate capabilities.

## Transport requests (API 0.2)

`plum.playback.request(options)` requires both `playback.read` and `playback.control`.
Only an explicitly invoked command or non-preview tool may call it. Loading, lifecycle,
observers, services and automatic previews cannot control transport, even by changing
Lua-visible phase/state fields. Permission is checked before queuing native effects.

| Plain options table | Native action |
|---|---|
| `{action="play"}` | start/resume if not already running, using existing native SoundFont/device policy |
| `{action="pause"}` | pause through native transport policy |
| `{action="stop"}` | native Stop, including its anchor behavior and note cleanup |
| `{action="seek",tick=integer_or_decimal_tick}` | exact absolute song tick, using existing native seek/navigation |
| `{action="speed",speed="1.5"}` | existing native decimal audition-speed parser/preferences; project tempo unchanged |

Options must be plain and contain exactly the fields allowed for that action.
Ticks use the standard checked tick parser. Speed must be native decimal text of
at most64bytes, not a Lua number or fraction expression. No immediate loop action is exposed by `request`; use the separate staged loop
completion API below for the persisted native setting.

```lua
plum.require_api(0,2)
plum.plugin.command {
  id="seek", name="Seek to authored position",
  run=function()
    local receipt=plum.playback.request{action="seek",tick=9007199254740993}
    print(receipt.request_id, receipt.snapshot.tick, receipt.snapshot.state)
  end
}
```

The worker waits on one bounded native request slot with cancellation/deadline checks
every10ms. Each attempted request uses a4096byte native reservation and counts toward
`plum.playback.max_requests_per_job` (32), including recoverable native rejections.
Native admission is at most4actions/second per plugin and8globally. Busy/rate-limited
requests are refused immediately with an actionable runtime error; they are not stored
for later surprise execution. Admission metadata is pruned and capped at16plugin IDs.

The native owner rechecks current active grants, enabled/current worker, live job,
exact project revision, native transport action generation and conflicting modal/native
work before acting. A native user seek/play/stop/loop/speed/policy change, output config/
route change, SoundFont loading or another accepted plugin action invalidates stale
queued requests. Natural playhead updates do not change that action generation.

A successful call returns `{request_id=1, snapshot=...}` with the actual native snapshot
after handling. The private host guard advances to that acknowledgement for subsequent
requests in the same run. `plum.playback.snapshot()` itself remains the original captured
run snapshot. Receipts indicate handling, not successful audible output: the native
state can be `audio_unavailable`. Native error/status reporting remains in use.

Accepted transport actions take effect immediately; cancellation afterward does not
roll them back. Pending expired/canceled/revoked requests have no effect. They create
no notes, MIDI events or Undo entries; speed preferences persist through the native
toolbar behavior. This is not a musical transaction or MIDI scheduler. No Lua runs
on the rendering/audio thread. **Transport Controls** is the installable/embedded
example for these five commands; it saves only its own acknowledgement preset.

## Staged loop settings (API 0.2)

`plum.playback.stage_loop(options)` requires `playback.read` and `playback.control`
in an explicitly invoked standalone plugin command. It accepts one of:

```lua
local intent=plum.playback.stage_loop{start=0,finish=3840,enabled=true}
-- Or preserve a disabled range: {start=0,finish=3840,enabled=false}
-- Or clear the range: {clear=true}
assert(intent.staged) -- queued intent; this is not an apply acknowledgement
```

`enabled` defaults to true. Tick bounds are exact and `finish > start`; options are
strict plain named-field tables. `clear` must be true and appear alone. The returned
fresh table contains `staged=true`, `enabled`, and range `start`/`finish` when present.
Changing it cannot change the native intent. Only one loop intent is allowed per run;
it consumes a4096byte native reservation and an attempt from the shared32control budget.

The host seals the intent only after successful uncanceled execution. Its finite native
data holds no World/document snapshot. The manager releases the completed job and gives
the native owner a turn before dispatching another worker snapshot. The owner checks
current grants, lifecycle, deadline, exact project revision, transport action/configured
loop guard, rate limits, modal work and exclusive document ownership. Refusal discards
the intent and reports a native status/plugin-log warning; it never silently clones
the note core or retains an unbounded retry queue. Another active project reader can
cause a busy refusal; invoke again after that work completes. Intents are consumed once.

Failed/canceled jobs produce no loop intent. Cancellation, reload, revocation, project
replacement, a newer run or competing native action invalidates old completion tokens;
a reused cancellation flag cannot revive one. Observation/lifecycle/service jobs and
automatic previews have no control authority. An open or committed musical transaction
prevents staging; a staged loop prevents starting a later musical transaction. Use
separate commands rather than implying a combined atomic musical edit.

Applied loops use the existing native project loop setting: set/disable/clear updates
the toolbar/ruler/transport, marks the project changed, persists in `.lup`, and remains
outside musical Undo exactly as native loop editing does. No notes or exported musical
content are generated. `configured_loop` preserves disabled ranges in snapshots/events;
`loop` describes the active native transport range. Transport-only observation still
uses bounded worker delivery. Transport Controls includes Stage Loop Range and Clear
Loop Range examples whose messages explicitly say *staged*.

Source/native integration and persistence are tested separately from interactive Windows
acceptance. The final Windows application gate still covers this API.

<a id="bounded-midi-event-plans-api-02-development"></a>

## Bounded MIDI event plans (API 0.2)

`plum.playback.plan_writer{max_events=N}` prepares channel-voice MIDI data on the
Lua worker. This offline preparation needs no playback permission and has no output
or project side effect. Building/reading a plan does **not** schedule or emit MIDI.
Explicit `plan:stage{output="internal"}` uses the separate permissioned native output
path below; no raw audio/DSP access is exposed.

```lua
local writer = plum.playback.plan_writer{max_events=4}
writer:append{
    tick={0,0,480,480},
    status={0xC0,0x90,0x80,0xE0},
    data1={0,60,60,0},
    data2={0,100,0,64},
}
local plan = writer:finish()
local page = plan:read{offset=0,limit=128}
assert(page.count == 4 and page.status[2] == 0x90)
assert(page.tick[3] == 480 and plan:info().finished)
plan:close()
```

Every batch is a plain named-field table containing four plain dense equal-length
arrays: `tick`, `status`, `data1`, `data2`. Unknown fields, holes, metatables and
non-integer bytes are errors. Ticks use the existing exact checked tick rules,
including integer/decimal-string values above2^53 up to2^63-1. Append ticks must
be nondecreasing across all batches. Equal-tick events retain author insertion order;
the host neither sorts nor invents musical behavior. Status0x80..0xEF is supported:
note off/on, polyphonic pressure, CC, program, channel pressure and pitch bend.
The low status nibble is the MIDI channel0..15. Data bytes are0..127; program and
channel pressure require canonical `data2=0`, using native message-length rules.
CC120..127 (channel-mode messages), system messages, meta and SysEx are rejected.
A note-on with zero velocity retains its exact bytes. Notes here use MIDI keys0..127;
this ephemeral MIDI representation does not alter project128/256-Key Mode.

`writer:append(columns)` validates the entire bounded batch in accounted scratch
space before appending and returns total count. A rejected batch preserves previous
records and leaves the writer usable. `writer:info()` returns fresh `count`,
`max_events`, `finished=false`, and optional exact `first_tick`/`last_tick`.
`writer:finish()` seals it and returns an immutable plan (empty plans are inspectable).
The finished writer cannot append, finish or cancel again. `writer:cancel()` eagerly
releases an unfinished buffer and is idempotent until job expiry.

`plan:info()` returns the same facts with `finished=true`. `plan:read([options])`
returns fresh packed columns plus `offset`, `count`, `total`, and `next_offset`
(nil at end). Offset is a zero-based integer0..count, default0; limit is an integer
1..4096, default4096. Reading offset=count returns empty columns. Editing returned
tables never changes native records. `plan:close()` releases the buffer and is
idempotent until expiry. Further reads/info fail clearly.

Limits:1..262144 reserved events per writer,1..4096 events per append/page,
1048576 cumulative reserved event slots and32 cumulative resources per job, including
canceled/closed/collected resources. Creation reserves native capacity plus512bytes
metadata before allocation; append separately reserves at most4096 native scratch
records. Plugin/process native budgets and worker cancellation/deadlines also apply.
All offline plan resources expire at job end, including failure/cancellation. A staged
native copy has the separate output lifecycle described below. The host eagerly
clears retained buffers; Lua handles still retain their small accounted metadata
until garbage collection. Plans cannot be stored for use in another worker job.
Use plugin-authored serializable settings or bounded data if regeneration is needed.

Source/worker tests cover strict validation, atomic batch rejection, stable ordering,
exact ticks, resource quotas, cancellation/expiry, and262144 native events with an
8MiB Lua heap. These tests are not realtime MIDI or physical device acceptance.

<a id="isolated-native-midi-plan-output-api-02-development"></a>

## Isolated native MIDI plan output (API 0.2)

`plan:stage{output="internal"}` requires **both** initially ungranted `playback.read`
and `playback.emit_midi` permissions. Installation never grants them. Use an explicitly
invoked standalone plugin **command**, with native playback already running, a loaded
SoundFont, and Realtime mode. The host neither starts playback automatically nor
writes generated notes into the project. Shared external MIDI routes and Pre-Render
are currently unsupported for plugin plans. Song, audition, and plugin voices are
separate; a plugin note-off/controller or cleanup cannot end or reset song voices.

```lua
-- Explicit command body; grant playback.read and playback.emit_midi first.
local writer=plum.playback.plan_writer{max_events=2}
writer:append{
    tick={plum.playback.tick_after(1.5),plum.playback.tick_after(2)},
    status={0x90,0x80},data1={60,60},data2={100,0},
}
local plan=writer:finish()
local intent=plan:stage{output="internal"}
assert(intent.staged and intent.output=="internal" and intent.count==2)
plan:close() -- staging owns an independent native prepared copy
```

`plum.playback.tick_after(seconds)` requires `playback.read` and a captured running
native output time map. It returns an exact Lua integer tick corresponding to the
captured playhead plus finite0..60seconds of native playback time. It uses the existing
TempoMap, including tempo ramps, SMPTE and audition speed, at native frame resolution.
Zero returns the exact captured tick. Calls in one run use the same captured position
and map; no live wall clock or Lua callback affects the result. Missing resources,
non-finite offsets or overflow fail clearly. Tick values remain subject to2^63-1.

Stage options are a strict plain table with exactly `output="internal"`. One MIDI
intent is allowed per job, counting toward the shared32playback-attempt limit. Open
or committed musical transactions prevent staging; staging prevents later musical
transactions, loop staging or transport requests in that command. Loop staging also
prevents MIDI staging. A successful run seals a root-free one-shot native intent.
Failed/canceled/superseded jobs publish none. The manager gives the native owner a turn
before dispatching another run; the owner checks current grants, enabled/ready lifecycle,
project/revision, native action/loop state, modal work, request rate limits and output.
Only one plugin MIDI stream can be active globally; busy work is refused, without
retries or an unbounded queue. A rejected intent reports a native/plugin-log warning.

`staged=true` acknowledges preparation only. After owner queueing, inspect
`plum.playback.snapshot().midi_output`: `idle`, `queued`, `running`, `finished`,
`canceled` or `refused`. `running` means the native timeline owns the schedule,
including any wait until its first event; it does not certify physical audibility.
`midi_pending` is the number of events not yet dispatched by the native render worker,
0..262144, and becomes0 on terminal cleanup. It does not count already mixed ring audio.
These global native stream facts are readable with `playback.read` and participate in
bounded transport observations. Read-only inspections/observer delivery preserve an
accepted stream; they cannot restart it.

The plugin worker compiles and prepares a dedicated128voice native synthesizer with
SoundFont samples shared and effects disabled. It reserves a conservative32MiB native
charge plus compiled event storage and4096bytes before construction; source storage
remains separately charged while alive. No Lua state, function, raw DSP buffer or
native pointer crosses to the audio path. Native submission uses the existing bounded
64command queue and master output, with a small boxed message; no UI wait occurs.

Additional scheduled-plan checks:1..262144events, span at most60seconds, at most64events
in **any sliding64-frame window**, at most128simultaneously held channel/key pairs,
no overlapping note-ons for the same channel/key, and a matching off for every on.
Zero-velocity note-on counts as an off. The native boundary independently validates
all safe statuses/bytes/order. At actual worker admission the first event must be
250ms..5seconds ahead of its current render clock; the complete plan including a
half-second release tail must fit the current playable project end. Source/sample
rate/SoundFont/tempo identity and non-prerender running state must still match.
Late/out-of-range/changed output gives `refused`, without sending partial events.
Internal rendering uses native64-frame timing blocks; scheduled note-off velocity
uses the existing internal sink behavior. Timed external routing is not exposed here.

`plum.playback.cancel_midi()` requires the same two grants and explicit invocation.
It returns `{cancellation_requested=true}` and invalidates only this plugin's MIDI
epoch, without stopping song playback. It is subject to the shared32attempt budget.
Native Cancel for an idle plugin also cancels its accepted stream. Global script
cancellation, disable/reload/removal/revocation, project changes, Stop/Pause/seek,
speed/tempo/output/SoundFont/loop changes and natural loop wrap clean up plugin voices.
Cancellation epochs survive reused worker flags; accepting a later read-only job
cannot revive canceled output. A canceled/finished schedule never resumes implicitly.

The native worker checks lifecycle even while its render ring is full and releases
owned buffers/voices without waiting for another rendered block. Previously mixed
samples remain for the existing bounded ring lookahead (normally about85ms at48kHz;
large callbacks can extend it within the native ring limit). No instantaneous unmixing
is promised. Native song traffic is not suppressed by plugin admission or cleanup.
There are no project-note, `.lup` or musical Undo entries; persisted editing uses the
existing transaction API separately. Plan/preset data and live output are distinct.

**MIDI Pattern Laboratory** is an installable/embedded author-written eight-note
pattern, exact time mapping, own preset, cancellation and state example. Native tests
exercise real RustySynth/headless ring samples, same-key song isolation, authority and
lifecycle/refusal/cleanup. Headless test devices and recorder sinks do **not** establish
actual Windows audio hardware, external MIDI devices, dense jitter/RSS or final release
acceptance. Those gates remain separately tracked.

<a id="live-midi-input-api-02-development"></a>

## Live MIDI input (API 0.2)

Declare `"midi.input"` in the plugin manifest and have the user grant it. Discovery
and opening require an explicitly invoked command/non-preview tool. Loading,
automatic observations, format adapters and pure service callbacks cannot obtain
input authority. The host always asks the user to choose and approve a device for
each `open`; discovery indices, names, presets and prior choices never open one.
No other capability implies `midi.input`. Live input is currently Windows WinMM only.

| Call | Result and limits |
|---|---|
| `plum.midi_input.devices()` | `{devices={{index=1,name="…"},…},reported=n,unavailable=n,truncated=bool}`; at most 256 native slots, skipped unavailable slots retain their original one-based index; at most four calls/job |
| `plum.midi_input.open([options])` | fresh native chooser/approval, then a stream; `nil` on chooser Cancel; optional plain `{capacity=1024}`, integer 1–4096; at most four open attempts/job, including canceled choices |
| `input:read([maximum [, wait_ms]])` | packed batch below; maximum integer 1–256, default 256; wait integer 0–1000 ms, default 0; waits only the Lua worker, checking cancellation at most every 10 ms between native operations |
| `input:info()` | current cumulative statistics below plus approved display `name` |
| `input:close()` | immediately revokes event delivery and requests native cleanup; `true` once, then `false`; no blocking join; later reads/info raise `expired` |

A read result is `{count=n,columns={…},stats={…}}`. Every column is a dense,
one-based array of exactly `count` values. Lua receives independent data; changing
it cannot change the native queue. The columns are:

| Column | Meaning |
|---|---|
| `sequence` | increasing native admission sequence; queue/rate drops create gaps; invalid/contended packets have separate counters |
| `driver_ms` | original unsigned 32-bit WinMM milliseconds since input start |
| `timestamp_ms` | extended integer driver milliseconds, preserving late timestamps and rollover; never sorted or clamped to arrival order |
| `received_ns` | integer host receipt nanoseconds since this capture context was created, before native open; not an audio sample clock or musical tick |
| `out_of_order` | extended driver time is below the previous high-water mark |
| `driver_overrun` | WinMM delivered this packet as `MIM_MOREDATA` |
| `status`, `data1`, `data2` | original status and valid data bytes; unused data bytes canonicalize to zero |
| `kind` | `note_on`, `note_off`, `poly_pressure`, `cc`, `program`, `channel_pressure`, `pitch_bend`, `clock`, `start`, `continue`, `stop`, `active_sense`, `reset` |
| `channel` | zero-based channel 0–15, or -1 for realtime messages |
| `note_on_zero` | original status was Note On with zero velocity; `kind` is `note_off`, original status remains available |

For Pitch Bend, the signed value is `data1 + 128*data2 - 8192`. Channel messages
include all valid CC numbers; host input does not automatically route sound, control
transport, edit notes or invent mappings. The plugin author supplies those algorithms
through separately granted public services. Running status is expanded by WinMM.
SysEx and other system-common/undefined statuses are unsupported. No SysEx buffer or
pointer reaches Lua; rejected packets count as invalid where a callback is received.

Statistics contain exact nonnegative integers `seen`, `accepted`, `queue_dropped`,
`rate_dropped`, `invalid`, `contended`, `driver_overruns`, `clock_regressions`, `queued`,
`capacity`, plus `status`: `open`, `closed`, `disconnected`, `exhausted` or `failed`.
`seen` counts serialized valid admission attempts, including rate/queue rejects;
invalid/contended attempts are counted separately. Counters are cumulative snapshots;
concurrent callback counters can advance while the worker reads other counters.
`accepted` is admission, not a promise that canceled queued packets will be delivered.
Closed/disconnected queues return zero events. A job delivers at most 1,048,576 events
across all its streams. Integer values beyond Lua's signed 64-bit range fail exactly
rather than wrapping or rounding, as with ticks elsewhere in this API.

Each queue is preallocated, drops newest when full and uses an integer token bucket
of 32,768 events/second with a 4,096-event burst. Callback lock contention drops the
new packet and increments `contended`; the callback never waits for a Lua drain.
Treat any increased loss/overrun counter as a discontinuity: clear author-held note,
clock or controller state as appropriate. A dropped Note Off must not leave a plugin's
separately authored playback mapping holding a note. Native input itself holds no
playback voices. Store the captured columns/seed/mapping inputs when implementing
deterministic replay or Preview/Apply; do not reread a live device during Apply.

Streams expire eagerly at job end, including handles retained in Lua globals. Cancel,
timeout, reload, disable, grant revocation, project revision/replacement/close and app
shutdown revoke access. Native ownership is separate from Lua/UI/audio: at most eight
I/O tasks with 1 MiB stacks and 2 MiB/task reservations, four device contexts, existing
per-plugin/global native caps, 10 ms lease checks and 250 ms connection probes.
Slow/stalled driver calls retain their task budget until returning; cancellation does
not wait for them. Cleanup revokes before stop/reset/close. A failed close retains its
callback context, native reservation and device slot until process exit, bounded by
four slots. Legacy equal device descriptors cannot prove persistent physical identity;
there is no automatic reconnect or remembered per-device consent. Detectable native
handle/descriptor disappearance closes the stream. Physical timing/unplug compatibility
remains a release acceptance test, not a claim from manufactured packets.

The installable [MIDI Input Laboratory](../crates/plumspirit/examples-lua/midi-input-laboratory/README.md) demonstrates bounded capture and deterministic project/preset replay using author-written note, CC and Pitch Bend mappings. Its source is the algorithm; the host supplies approved input and native transactions.

This command body is executed by the native conformance tests with manufactured input
and controlled consent. It does not alter the project:

<!-- midi-input-example:start -->
```lua
plum.require_api(0, 2)
local inventory = plum.midi_input.devices()
if #inventory.devices == 0 then return 0 end
local input = plum.midi_input.open{capacity=1024}
if not input then return 0 end
local batch = input:read(64, 250)
local note_ons = 0
for i = 1, batch.count do
  if batch.columns.kind[i] == "note_on" then note_ons = note_ons + 1 end
end
local s = batch.stats
if s.queue_dropped + s.rate_dropped + s.invalid + s.contended + s.driver_overruns > 0 then
  print("MIDI input discontinuity; reset author-held input state.")
end
input:close()
return note_ons
```
<!-- midi-input-example:end -->

## Selected files (API 0.2)

Only an explicitly invoked command or non-preview tool run may open a dialog.
Loading, lifecycle hooks and automatic Preview runs cannot prompt. Native dialogs
belong to an application-managed UI thread; Lua waits on its worker, while the app
polls without blocking its frame/audio loop. One native picker runs at a time, with
at most eight requests per job. The command's time limit includes time in the picker.
On Windows, canceling the job, disabling/reloading the plugin, changing/closing the
project or exiting closes/invalidates its picker and file grants.

| Call | Returns / behavior |
|---|---|
| `plum.files.open([options])` | read-only opaque file userdata, or `nil` when the user cancels; requires `file.read_user_choice` |
| `plum.files.save([options])` | staged write-only userdata, or `nil` on cancel; requires `file.write_user_choice`; native Save confirms an existing destination |
| `f:info()` | `{name, size, offset, mode}`; basename only, byte size/offset as integers, mode `"read"` or `"write"`; no absolute path |
| `f:read([bytes])` | binary Lua string from the current offset; default 65,536, maximum 1 MiB; `""` at EOF; read handles only |
| `f:read_all([maximum])` | remainder as a binary string; default/hard maximum 8 MiB; an oversized remainder errors without advancing the offset |
| `f:seek(offset)` | exact integer byte offset from 0 to file size; returns offset; read handles only |
| `f:write(bytes)` | append a binary string of at most 1 MiB, returning its byte count; output remains private staging |
| `f:commit()` | flush/sync and atomically publish the staged file, returning `true`; closes the handle; write handles only |
| `f:close()` | close/discard without publication; idempotent; no return value |

Options are `{name="output.txt", filters={{name="Text", extensions={"txt","csv"}}}}`.
`name` is Save-only, a safe basename of at most 240 characters/960 UTF-8 bytes.
Filters: at most eight, each with a 1–64-character printable label and 1–16
extensions (ASCII letters/digits, at most 16 bytes each). All Files is also offered.
Filters are hints, not format validation. No option accepts paths, directories or
custom dialog titles. Unknown options fail before the picker. Text fields must be
UTF-8; `read`/`write` preserve arbitrary binary bytes, including NUL and newlines.
Validate text with `utf8.len` or a format parser in your authored algorithm.

Constants: `plum.files.max_chunk` (1,048,576), `max_read_all` (8,388,608),
`max_job_bytes` (1,073,741,824). The selected input must be at most 1 GiB. Cumulative
read bytes and cumulative written bytes each have a 1 GiB per-job cap across all
handles, including repeated seeks. Native read buffers are reserved separately
from the Lua VM quota: 256 MiB per plugin (shared with reload candidates), 512 MiB
globally. Output staging is disk-backed; it does not accumulate a giant Lua string.
I/O loops check cancellation every 64 KiB. OS I/O/flush latency is cooperative,
not a guarantee that the OS will return in a fixed number of milliseconds.

Files are tied to plugin, job, consent and project snapshot. A retained userdata
is closed and expired when its job finishes, fails or is canceled, including when
Lua retains it in a global. Presets/config cannot persist a file grant. New jobs
must ask again. A file from another state cannot be forged or opened by path.

Windows local-disk implementation: parent directories are pinned without delete
sharing; reparse points, symlinks/junctions, device/UNC paths, alternate data streams
and unsafe reserved names are refused. At most 64 parent directories may be pinned per selected path. The opened input is held without write/delete
sharing. Writes use an exclusively opened same-directory staging object and rename
that actual handle; an existence/identity/size/modification check rejects a changed
destination before publication. Failed/abandoned staging leaves the destination
unchanged. Atomicity is that of the underlying local filesystem; cross-volume
publication is never requested. Other platform GUI/filesystem race acceptance has
not been verified.

**Publication is explicit.** Before `commit`, error/cancellation/close/job completion
removes staging. A completed `commit` is an independent filesystem save, outside
project Undo; a later Lua error or canceled project edit cannot undo that save.
Close the input before committing if the user deliberately selected the same file.
There is no multi-file/project-plus-files atomic transaction.

Permission denial is `capability`; stale/closed grants are `expired`; quota excess
is `budget`; timeout/cancellation retain their existing categories. OS file failures
are `runtime` with a `file_io:` subcode and OS kind/code, without private paths.
Invalid arguments are `runtime`; a canceled selection is ordinary `nil`.

```lua
plum.require_api(0, 2)
-- Inside a registered command, with both file capabilities granted:
local input = plum.files.open()
if not input then return end
local output = plum.files.save{name="copy.bin"}
if not output then input:close(); return end
while true do
  local bytes = input:read(65536)
  if #bytes == 0 then break end
  output:write(bytes)
end
input:close()
output:commit()
```

The installable [Selected Text File sample](../crates/plumspirit/examples-lua/file-transform/README.md)
validates streamed UTF-8 across chunk boundaries and implements its own ASCII
uppercase algorithm. It does not mutate notes or automatically generate tools.

## Unicode text (API 0.2)

`plum.text` currently implements bounded **extended grapheme segmentation** using
the already-locked unicode-segmentation 1.13.3 library and its Unicode 17.0.0 data.
It preserves exact UTF-8, including combining marks, variation selectors, skin tones,
ZWJ sequences, regional-indicator flags, keycaps, CRLF and embedded NUL. No
normalization, case conversion or replacement character is inserted. These are pure
worker computation services with no additional permission or filesystem access.

| Call | Signature and result |
|---|---|
| `plum.text.grapheme_count(text)` | integer count `0..65536`; validates the whole string under the limits below; empty text returns 0 |
| `plum.text.graphemes(text)` | iterator function: `for cluster,first,after,index in plum.text.graphemes(text) do ... end`; cluster is the exact original substring; first/after are **one-based byte offsets**, after exclusive; index is one-based grapheme order |
| `plum.text.unicode_version` | actual segmentation data version string, currently `"17.0.0"` |

For every returned range, `text:sub(first,after-1)==cluster`. The last after-offset is
`#text+1`. An exhausted iterator returns nil; repeated calls during that job stay nil.
Two new iterators advance independently; aliases of one function share its cursor.
Grapheme results describe the input text. Separate font, shaping and geometry APIs
below report actual glyph support and positions; segmentation alone establishes no
rendering support. Remaining layout/fallback/color paths stay tracked under
UNI-01/TEXT-01/TEXT-02.

Limits are discoverable on `plum.text`:

| Constant | Value and meaning |
|---|---|
| `max_bytes` | 1,048,576 UTF-8 bytes per input string |
| `max_cluster_bytes` | 65,536 bytes per extended grapheme; an oversized cluster fails instead of being split |
| `max_graphemes` | 65,536 graphemes per scan |
| `max_iterators` | 64 iterator creations per job, including exhausted/abandoned ones |
| `max_scan_bytes` | 8,388,608 charged forward/context bytes per scan; pathological context work fails usefully |

Inputs must be Lua strings containing valid UTF-8; numbers are not coerced and invalid
bytes fail with `runtime`, without lossy decoding. Input-byte size is checked before
UTF-8 validation. Counting validates all clusters; a streaming iterator validates
cluster/count/work limits as consumed, so an error may follow a valid prefix.
Byte/cluster/count/work/iterator/native-reservation limits produce `budget`. An
allocation failure in Lua produces `memory`. Cancellation/timeouts retain their
categories. Processing/allocation failures close the iterator and subsequent same-job
calls return nil. A rejected reentrant call leaves the active outer cursor intact.

The host validates UTF-8 once and pins a safe immutable view of the existing Lua
string; it does not copy the whole string into a native buffer or build a full cluster
table. Small cursor state is charged to the shared 256 MiB per-plugin / 512 MiB global
native pool; input and returned strings obey the Lua VM quota. Counting uses only
bounded cursor state on the stack and creates no iterator resource.

Scan chunks use 4 KiB forward slices with at most one scalar of lookbehind; requested
context slices have at most three extra bytes for UTF-8 alignment. Cancellation and
deadline checks occur between chunks, boundaries and context requests, including
inside a long combining cluster. Charged work sums forward cursor advancement and
supplied context lengths; the job time budget also bounds processing.

Iterators belong to the current plugin/job. End-of-input, failure and job/lifecycle
completion release pinned views and native reservations, even when the iterator is
retained in a Lua global. A call in a later job raises `expired`; store ordinary string
results if needed across runs. Original string values remain ordinary Lua data. The
font/geometry/transaction services and authored Text Art Studio commands compose
with these boundaries. Remaining typography/fallback/color acceptance is tracked
separately; every original API0.1 text/curve/plugin surface remains supported.

```lua
plum.require_api(0,2)
local source = "∑👍🏽👨‍👩‍👧‍👦"
assert(plum.text.grapheme_count(source)==3)
local expected = {"∑", "👍🏽", "👨‍👩‍👧‍👦"}
for cluster,first,after,index in plum.text.graphemes(source) do
  assert(cluster==expected[index])
  assert(source:sub(first,after-1)==cluster)
end
```

<a id="script-runs-for-mixed-script-shaping-api-02-development"></a>

### Script runs for mixed-script shaping (API 0.2)

`plum.text.script_runs(text,{max_runs=4096}?)` returns original `text`, `count`, and
packed `first`, `last`, `script`, `resolved` columns in **logical source order**.
Byte ranges are 1-based inclusive first/exclusive last, cover the complete original
string exactly once, and never split an extended grapheme. Empty input has zero runs.
`script` uses four-letter ISO15924 tags; `resolved` identifies a specific resolved
script suitable for the existing `font:shape_run(...,{script=tag})` option.

Runs are maximal compatible intersections of Unicode Script_Extensions over whole
graphemes. Common/Inherited characters join compatible context: leading neutrals
join following text, trailing neutrals stay with preceding text. A strong primary
script is preferred when compatible; a unique remaining extension also resolves.
A Common base with inherited controls stays Common. An ambiguous set or conflicting
scripts inside one indivisible grapheme reports
`Zzzz,false`; standalone Common/Inherited reports `Zyyy,false`/`Zinh,false`. Private-use
or unknown characters remain unresolved. The service never invents a language, font
coverage or replacement glyph. Paired punctuation is not specially matched across
script changes. Authors can retain unresolved metadata, choose explicit runs, or use
native shape guessing for those pieces. This is script segmentation, not full text
layout or language/font fallback.

Uses Unicode-script0.5.8/Unicode17.0.0, reported by `script_unicode_version`.
Strict UTF8/16KiB/4096scalar input, preflighted native workspace, periodic cancellation
and atomic-output limits also apply. `max_runs` is a plain-table integer1–4096; no other option is
accepted. Result tables are ordinary persistable data. No new capability is needed.
See [Unicode Script_Extensions](https://docs.rs/unicode-script/0.5.8/unicode_script/struct.ScriptExtension.html)
and [UAX24](https://www.unicode.org/reports/tr24/tr24-39.html) for the underlying properties.

This author composes bidi visual order with script logical runs. It reverses the
**order of script runs** inside RTL pieces; it never reverses the text bytes.
The same technique can feed original glyph outlines or a note transaction.

```lua
local t=plum.text
local source="AΩЖ"
local font=t.builtin_font("proportional")
local bidi=t.bidi_runs(source)
composed={text=source,count=0,first={},last={},script={},glyph_id={},x={}}
local pen=0
for i=1,bidi.count do
    local part=source:sub(bidi.first[i],bidi.last[i]-1)
    local scripts=t.script_runs(part)
    for visual=1,scripts.count do
        local j=bidi.direction[i]=="rtl" and scripts.count-visual+1 or visual
        local offset=bidi.first[i]+scripts.first[j]-2
        local piece=part:sub(scripts.first[j],scripts.last[j]-1)
        local run=font:shape_run(piece,{direction=bidi.direction[i],
            script=scripts.resolved[j] and scripts.script[j] or nil})
        assert(run.missing_count==0,"Selected font does not cover the original text.")
        for k=1,run.count do
            local n=composed.count+1;composed.count=n
            composed.first[n]=offset+run.first[k];composed.last[n]=offset+run.last[k]
            composed.script[n]=scripts.script[j];composed.glyph_id[n]=run.glyph_id[k]
            composed.x[n]=pen+run.x[k]
        end
        pen=pen+run.advance_x
    end
end
font:close()
```

<a id="bidirectional-runs-and-unicode-line-breaks-api-02-development"></a>

### Bidirectional runs and Unicode line breaks (API 0.2)

`plum.text.bidi_runs(text,options?)` returns direction metadata for explicit hard lines
without reversing or normalizing the original string. It uses native Unicode-bidi
analysis, with Unicode16.0.0data reported by `bidi_unicode_version`. Options are a plain
table with `direction="auto"|"ltr"|"rtl"`, `max_runs` (1–4096, default4096) and
`max_lines` (1–256, default256). Auto resolves each hard line's base direction.

The result has original `text`, `count`, `line_count`, `has_rtl`, a `lines` array, and
packed run columns `first`, `last`, `line`, `level`, `direction`. Runs are in visual
order within each hard line. Each range is an original **1-based inclusive byte first /
exclusive byte last**; `text:sub(first,last-1)` is still in logical source order and
should be passed to `font:shape_run` with that run's direction. Levels are native
resolved bidi embedding levels; odd levels are RTL. Original byte offsets may descend
in visual order. No source bytes are discarded or rewritten.

Each line entry contains `first`, `content_last`, `last`, `level`, `direction`,
`first_run`, `last_run`. `first..last-1` includes its original hard separator;
`first..content_last-1` excludes it. `first_run..last_run-1` indexes the visual runs.
CRLF is one separator. Unicode B-class separators and VT/FF/NEL/LS/PS end hard lines.
Trailing and empty lines are explicit, including one empty line for empty text;
empty lines have no runs. The host refuses any native run boundary that would split
an extended grapheme. This service does not perform soft wrapping or full mixed-script
itemization, font fallback, glyph coverage, UAX9mark placement or glyph mirroring;
shaping handles the supported mark/mirroring work inside an appropriate authored run.

`plum.text.line_breaks(text,{max_breaks=4096}?)` returns original `text`, `count` and
packed `position`/`kind` columns. Positions are original1-based byte offsets **after**
the break, suitable for `text:sub(previous,position-1)`. Kind is `"allowed"` or
`"mandatory"`. Nonempty text ends with a mandatory opportunity at `#text+1`; empty
text has zero opportunities. CRLF is not split. Native UAX14opportunities are filtered
through the current UAX29grapheme boundaries, so a combining or emoji sequence is not
split. `max_breaks` is an integer1–4096; exceeding it fails instead of truncating.

Line breaking uses Unicode-linebreak's Unicode15.0.0data, reported by
`linebreak_unicode_version`; its default complex-context handling treats SA characters
as alphabetic and does not provide language-specific dictionary breaks. Segmentation
uses the separately reported Unicode17.0.0data. These distinct data versions are
intentional and do not establish full newer-Unicode layout conformance.

Both calls are pure worker computation without project/filesystem/UI permissions.
Inputs are strict valid UTF8strings, at most `max_layout_bytes`16384bytes and
`max_layout_scalars`4096scalars. `max_layout_lines`256 and `max_layout_runs`4096 are
discoverable. The native workspace is preflighted before analysis; input, boundary,
hard-line and output loops check cancellation, as do the boundaries of each bounded
native bidi phase. Native/VM/cap/cancel failures return no partial table. Returned
plain data can be saved, copied or retained across jobs; there is no native handle.

This small author-written example places two short shaped lines in em coordinates.
The plugin chooses baseline spacing and can map the resulting glyph outlines into
canvas, graphs or MIDI. Respect `shape_run`'s256scalar limit when splitting longer text.

```lua
plum.require_api(0,2)
local source="office é\r\nTwo lines"
local analysis=plum.text.bidi_runs(source)
local breaks=plum.text.line_breaks(source)
local font=plum.text.builtin_font()
layout={text=source,count=0,glyph_id={},first={},last={},x={},y={}}
for line_index,line in ipairs(analysis.lines) do
  local pen=0
  for i=line.first_run,line.last_run-1 do
    local part=source:sub(analysis.first[i],analysis.last[i]-1)
    local run=font:shape_run(part,{direction=analysis.direction[i]})
    for j=1,run.count do
      local n=layout.count+1
      layout.glyph_id[n]=run.glyph_id[j]
      layout.first[n]=analysis.first[i]+run.first[j]-1
      layout.last[n]=analysis.first[i]+run.last[j]-1
      layout.x[n]=pen+run.x[j]
      layout.y[n]=-(line_index-1)*1.5+run.y[j]
      layout.count=n
    end
    pen=pen+run.advance_x
  end
end
assert(breaks.kind[breaks.count]=="mandatory")
font:close()
```

For API details behind the native services, see the
[Unicode-bidi documentation](https://docs.rs/unicode-bidi/0.3.18/unicode_bidi/) and
[Unicode-linebreak documentation](https://docs.rs/unicode-linebreak/0.1.5/unicode_linebreak/).

## Images and pixels (API 0.2)

Images are immutable native resources. `plum.images.load(file)` accepts only a current,
read-only userdata returned by `plum.files.open` or `plum.plugin.asset`; it does not accept paths or create a
picker. It uses the same decoder as the built-in Image-to-MIDI tool. This reads actual
bytes from the approved handle, charges the cumulative selected-file read budget,
checks cancellation on read/seek, and restores the script's file cursor even on failure.
The input may be closed after loading; the decoded pixels remain usable in that job.

| Call | Signature, result and validation |
|---|---|
| `plum.images.load(file)` | immutable image userdata from an approved read handle; loading needs its `file.read_user_choice` authority; no additional image permission |
| `plum.images.from_rgba(width, height, bytes)` | image from a packed binary RGBA string; exact positive integer dimensions, exact `width*height*4` byte length; pure computation with no file permission |
| `image:info()` | `{width, height, format, bytes, origin="top_left", alpha="straight"}`; format is `png/jpeg/bmp/gif/webp/tiff/rgba`; bytes is retained native pixel storage |
| `image:pixel(x, y)` | four integer returns `r,g,b,a`, each 0–255; exact zero-based integer coordinates inside the image |
| `image:row(y [, x [, count]])` | packed binary RGBA bytes in left-to-right order; default x=0 and count=remaining width; rejects an empty or out-of-bounds row |
| `image:read{x=0, y=0, width=n, height=n}` | packed RGBA rectangle, rows top-to-bottom with no padding; positive width/height required, x/y default zero; out-of-bounds/unknown fields fail |
| `image:crop{x=0, y=0, width=n, height=n}` | new native image containing that rectangle; same exact integer coordinates as `read`, without the 1 MiB read-string limit; positive size, entirely inside source; unknown fields fail |
| `image:resize(width, height [, filter])` | new image with exact positive integer dimensions; filter is `"box"` (default) or `"nearest"`; other filters fail |
| `image:transform{rotate=0, scale_x=1, scale_y=1, flip_x=false, flip_y=false}` | new image using the native centered nearest-pixel transform; finite numbers and strict boolean flips; scale magnitudes must be at least 0.001; unknown options fail |
| `image:warp{matrix=m,width=n,height=n,edge="transparent"}` | new image on an explicitly sized canvas, using the forward affine matrix below and nearest inverse sampling; edge can also be clamp/wrap/mirror |
| `image:fit(width,height [,policy])` | two results: new image and its forward matrix; policy `"contain"` (default), `"cover"` or `"stretch"`; centered nearest sampling |
| `image:close()` | release pixels immediately; idempotent; no result |
| `plum.images.brightness(r,g,b)` | arithmetic mean of the three encoded channels, normalized to 0–1; inputs must be integer bytes |
| `plum.images.luminance(r,g,b)` | normalize sRGB bytes, linearize each (`v/12.92` up to 0.04045, otherwise `((v+0.055)/1.055)^2.4`), then `0.2126R+0.7152G+0.0722B`; output 0–1; alpha is not an input |

There are no setters for native pixels. To construct authored image data, create another
bounded RGBA resource. Crop/resize/transform leave the source unchanged and return
independent job-scoped handles with `format="rgba"`; even an identity creates a new
handle. Matching cached results may share immutable pixel storage. Closing either
source or result does not close another handle.
These are pure computation and require no additional capability.
Userdata equality follows Lua object identity; pixel equality is
not a permanent resource ID. Image resources belong to one plugin/job and its project
snapshot. They close on success, error, cancellation, unload or project closure. Retaining
a userdata in a global/preset does not preserve its pixels or consent into another job.
Queries after close/job completion fail with `expired`. No raw file/GPU/native pointers
are exposed and no Lua runs in a rendering/audio callback.

Supported decoding: PNG, JPEG, BMP, GIF, WebP, TIFF through the existing decoder features.
Content detection takes precedence; the approved basename provides the same format
fallback as the built-in converter, including BigTIFF files named `.tif`/`.tiff`.
Animated/multi-image inputs use the decoder's first frame/image; frame enumeration is
not exposed. Output is unpremultiplied RGBA8 with a top-left origin. Existing decoder
conversion to RGBA8 is preserved; no new ICC color management, EXIF autorotation or
profile conversion is applied. The luminance helper explicitly assumes sRGB inputs.
The API does not premultiply or zero decoded RGB bytes merely because alpha is zero;
actual codec conversions follow the existing decoder.

Limits are discoverable as `plum.images.max_pixels` (16,777,216), `max_file_bytes`
(67,108,864), `max_region_bytes` (1,048,576), and `max_handles` (64 image and mask
creations together per job, including resources later closed). Maximum side remains
16,384; the built-in converter
keeps its own larger historical input/pixel limits. Rectangle reads are capped at 1 MiB;
read large images as rows or explicit tiles, without one Lua call per pixel or a full
image Lua table. `from_rgba` requires its bounded source string in the VM; file decoding
does not duplicate compressed/pixel arrays into Lua.

The host reserves a metadata allowance of twice the encoded length plus 2 MiB and caps
metadata reads (including repeated seeks) to that allowance before dimensions are
accepted. Decoder metadata limits are separate from the pixel phase. It checks dimensions
and reserves two raw decoder buffers, RGBA conversion/storage and fixed scratch before
allocating output pixels; this covers codecs that retain an internal raw buffer alongside
DynamicImage storage. Codec payload limits are narrowed to that preflight. These are
conservative allowances and codec limits, not instrumentation of every allocator call.
Retained pixels and query buffers count against the same 256 MiB per-plugin / 512 MiB
global native budgets as other services, separately from the VM quota. Preflight may
refuse an image below the dimensional ceiling when other resources or decode workspace
exhaust that budget. Failed loads release reservations. Loop/stream checks are periodic;
opaque codec CPU calls are cooperatively checked before/after, not forcibly preempted.
The in-process decoder/interpreter residual risks described in the threat model remain.

Invalid coordinates/data/types/formats and corrupt decoder input produce `runtime`
errors (decoder messages, including decoder dimensional/codec limits, have a `decode:`
subcode). Encoded-size, native-allocation reservation, handle-count, region-byte and file
read quotas produce `budget`. Explicit RGBA/query buffer allocation failures produce
`memory`; codec allocation errors retain `runtime/decode`. Cancellation/timeouts retain
their existing categories.
Images, masks and image-note plans alone do not edit project notes, commit transactions
or change Undo state. Bounded note_plan/read methods are documented below; contour
outlines remain planned. Connected mask regions are also supported below.

Resize uses the same box filter as the built-in converter: each output pixel averages
the covered source area independently for all four straight RGBA channels and rounds
to the nearest byte (half upward). RGB under zero alpha still participates; this is
not alpha-weighted filtering. Nearest resize samples the source cell containing the
mapped destination pixel centre: `floor((x+0.5)*source_width/width)` and likewise Y.

Transform scales and mirrors around the image centre, then rotates clockwise by
`rotate` degrees, preserving the native converter's mapping. A negative scale and the
corresponding flip toggle each other. With scaled dimensions `sw`, `sh`, output width
is `max(1,round(sw*abs(cos(a))+sh*abs(sin(a))))`; height exchanges sine/cosine. Extents
use nearest-integer rounding, as in the existing converter, rather than a guaranteed
outer-ceiling bound. The inverse-mapped destination pixel centre selects a source cell
by floor; outside samples are transparent black. There is no smoothing, custom pivot
or implicit color conversion. Finite overflowing/extreme transforms are rejected by
the dimensional bounds before allocation.

All derived outputs obey the 16,384-side / 16,777,216-pixel / 64-successful-creation
limits. The native quota must simultaneously cover source, output and any workspace;
an output smaller than the pixel ceiling may still fail with `budget`. Box-resize
preflight includes cached source-span vectors and entries, then releases that scratch
reservation when only the output pixels remain. Crop, nearest resize and transform
allocate only their output pixels. All loops check cancellation at least per output
row; box filtering also checks every 65,536 accumulated source samples, including
when reducing a large source to a single output pixel. Explicit allocation failures
produce `memory`, invalid options/scales/coordinates/native dimensional bounds produce
`runtime`, plugin pixel/native/handle quotas produce `budget`. Failure creates no
resource and leaves the source unchanged. No renderer or audio callback executes Lua.

### Affine image mapping and cache (API 0.2)

A forward image-space matrix is a plain table with exactly six finite fields:
`{a=...,b=...,c=...,d=...,e=...,f=...}`, meaning `x'=a*x+b*y+e`, `y'=c*x+d*y+f`.
Coordinates describe pixel **edges**, with top-left outer edge `(0,0)` and source
extent `[0,width]` / `[0,height]`. These are floating image coordinates, not exact
absolute MIDI ticks. Matrices contain serializable author data, without native tokens.

| Call | Result and rules |
|---|---|
| `plum.images.mapping{translate_x=0,translate_y=0,scale_x=1,scale_y=1,rotate=0,pivot_x=0,pivot_y=0}` | forward matrix: scale around pivot, rotate clockwise around pivot, then translate; negative scales mirror; unknown options fail |
| `plum.images.compose(first,second)` | forward matrix applying first, then second; reuses native affine composition |
| `plum.images.inverse(matrix)` | inverse matrix; singular/unstable/out-of-range matrices fail |
| `plum.images.map_point(matrix,x,y)` | two numeric returns: forward mapped X/Y; use the inverse matrix for destination-to-source lookup |

All coefficients, mapping parameters, point inputs/outputs and inverse coefficients
must be numeric, finite and within +/-1e12. Inversion normalizes the linear 2x2 part
by its largest absolute coefficient and rejects normalized determinant magnitude
<=1e-12. Zero scales, singular and ill-conditioned matrices, overflow, unknown keys,
missing matrix fields and numeric strings produce `runtime`. Small floating-point
rounding remains possible; source pixel data is unchanged by constructing a matrix.

`image:warp` requires positive integer output width/height within image limits. It
checks inverse-mapped canvas corners before allocating output, refusing coordinates
outside +/-1e12. Each destination pixel centre `(x+0.5,y+0.5)` maps through the inverse;
the floored source coordinates select one pixel. No smoothing or implicit color
conversion occurs. Output is an immutable RGBA resource, including copied RGB under
transparent source pixels. Edge handling is explicit:

- `transparent`: outside source bounds becomes transparent black.
- `clamp`: clamp floored indices to the nearest edge pixel.
- `wrap`: floored indices modulo the respective image dimension.
- `mirror`: floored indices modulo twice the dimension, reflecting the second half;
  edge pixels repeat (for width 2, indices -2,-1,0,1,2,3 select 1,0,0,1,1,0).

`image:fit` returns its reusable matrix as a second result. Contain uses the smaller
X/Y scale and transparent letterboxing; cover uses the larger scale and clips excess;
stretch scales X/Y independently. All align source and destination centres. Sampling
follows the same pixel-centre rule, including half-pixel alignment on odd sizes. The
returned matrix can be inverted for exact same numerical coordinate interpretation or
passed to `warp` on the same canvas. `fit` uses transparent edge handling.

Crop, resize, centered transform, warp and fit use a bounded per-job cache of at most
`plum.images.max_cache_entries` (16) weak entries. Keys include source resource identity,
operation and all numerical settings/filter/edge/canvas dimensions. Cache hits create
new handles but reuse one immutable buffer and its native reservation; logical
`info().bytes` may describe storage shared with another handle. Oldest entries are
evicted, and dead entries are pruned. Weak entries do not keep pixels alive after the
last handle closes or is collected. Eager end-run/lifecycle cleanup clears the cache
and all handles even when Lua globals retain them. Nothing is cached across jobs or
input identities; there is no persistent consent token or permanent resource ID.

Every hit still consumes the shared 64-creation budget and validates the current job/
source. A miss preflights output/workspace against native quotas. Warp checks
cancellation per output row. Cache eviction or closing another handle cannot invalidate
a live result. These are functional resource guarantees, not measured slider/frame
latency or peak-RSS claims. Matrices need no capability; resource operations retain the
same expiry/error rules as other image services.

```lua
local matrix = plum.images.mapping{rotate=90,pivot_x=1,pivot_y=1,translate_x=2}
local x,y = plum.images.map_point(matrix,0,0)
local sx,sy = plum.images.map_point(plum.images.inverse(matrix),x,y)
assert(math.abs(sx)<1e-12 and math.abs(sy)<1e-12)
local image = plum.images.from_rgba(2,1,string.char(255,0,0,255,0,255,0,128))
local fitted,map = image:fit(4,4,"contain")
local same = image:warp{matrix=map,width=4,height=4}
assert(fitted~=same) -- independent handles, immutable storage may be shared
fitted:close()
assert(same:info().width==4)
same:close(); image:close()
```

```lua
plum.require_api(0, 2)
-- In a registered command with file.read_user_choice:
local file = plum.files.open{
  filters={{name="Raster images", extensions={"png","jpg","bmp","gif","webp","tiff"}}}
}
if not file then return end
local image = plum.images.load(file)
file:close()
local info = image:info()
local r,g,b,a = image:pixel(0,0)
local first_row = image:row(0)
local corner = image:read{x=0,y=0,width=1,height=1}
local y = plum.images.luminance(r,g,b)
local mean = plum.images.brightness(r,g,b)
plum.ui.notify(string.format("%d by %d; first alpha %d; luminance %.3f, mean %.3f",
                            info.width, info.height, a, y, mean))
image:close()
```

### Masks and color analysis (API 0.2)

These are native computation services with no additional permission. RGB inputs are
three exact integer bytes; alpha is not an argument to the scalar color helpers.

| Call | Result and rules |
|---|---|
| `plum.images.rgb_to_hsv(r,g,b)` | three numbers: hue in degrees `[0,360)`, saturation and value in `[0,1]`; achromatic hue is zero |
| `plum.images.rgb_to_hsl(r,g,b)` | hue in degrees `[0,360)`, saturation and lightness in `[0,1]`; achromatic hue is zero |
| `plum.images.hsv_to_rgb(h,s,v)` | three integer bytes; finite numeric hue `0..360`, saturation/value `0..1`; 360 equals zero; round to nearest byte, half upward |
| `plum.images.hsl_to_rgb(h,s,l)` | same hue and rounding rules, finite saturation/lightness `0..1` |
| `plum.images.color_distance(r,g,b,r2,g2,b2)` | `sqrt((dr²+dg²+db²)/(3*255²))`, normalized `0..1` in encoded RGB byte space |
| `plum.images.nearest_color(palette,r,g,b)` | two returns: **one-based** nearest palette index and normalized distance; uses native converter squared-RGB selection, first minimum wins |
| `image:histogram([options])` | bounded regional channel counts and sampling metadata, described below |
| `image:mask(options)` | immutable one-byte-per-pixel coverage mask with the source dimensions; described below |

HSV/HSL conversions use encoded RGB channels, with no profile conversion. Inverse
conversion rejects non-finite numbers, out-of-range values and numeric strings.
`palette` must be a dense sequence of 1–1,024 colors, each exactly `{r,g,b}` with integer
bytes; holes, extra keys and extra components fail. Its temporary native storage is
quota-accounted. `plum.images.max_palette` is 1,024.

`image:histogram{channel="luminance", bins=256, max_samples=262144, alpha_min=...,
x=0, y=0, width=..., height=...}` accepts only those keys. X/Y are exact zero-based
integers; width/height default to the remainder of the image and must be positive and
inside it. Channels are `red`, `green`, `blue`, `alpha`, `luminance`, `brightness`, `hue`,
`saturation`, `value`, `lightness`. Saturation/value use HSV; lightness uses HSL. All
channels are normalized `0..1` for binning, with hue divided by 360.

Bins must be `1..plum.images.max_histogram_bins` (1,024); samples must be
`1..plum.images.max_histogram_samples` (262,144). For region size N, examine
`min(N,max_samples)` pixels at exact row-major indices `floor(k*N/examined)` for
`k=0..examined-1`. Alpha rejection happens **after** choosing those indices: alpha
below `alpha_min` is excluded. The default is 0 for the alpha channel and 1 for all
other channels. Accepted pixels each add one count, without alpha weighting, to
`min(bins-1,floor(value*bins))`. Transparent RGB can be explicitly examined with
`alpha_min=0`. Sampling and bin order are deterministic.

The result is `{counts={...}, samples=n, examined=n, pixels=N, estimated=bool,
channel=..., x=..., y=..., width=..., height=...}`. Counts are exact for the examined
accepted samples; they are not extrapolated to unsampled pixels. `estimated` is true
when examined is less than N. Count-vector native storage and the small output Lua
table obey their respective quotas. The service never creates a full image Lua table.

Mask options are discriminated by `kind` (default `"alpha"`). Unknown or inapplicable
options fail. `invert` is always a strict boolean and defaults false.

| Kind and accepted options | Coverage calculation before byte rounding |
|---|---|
| `"alpha"`, optional `min`, `max`, `invert` | normalized source alpha; without thresholds the output byte equals source alpha |
| `"luminance"` or `"brightness"`, optional `min`, `max`, `invert`, `alpha_min`, `preserve_alpha` | the documented scalar helper applied to source RGB |
| `"color"`, required `color={r,g,b}`, optional `tolerance`, `invert`, `alpha_min`, `preserve_alpha` | 1 when normalized RGB distance is at most tolerance, otherwise 0; tolerance defaults 0 and must be finite `0..1` |

For scalar kinds, supplying `min` or `max` replaces the continuous value with an
inclusive range test (1 inside, 0 outside); omitted endpoints default to 0 and 1.
Endpoints must be finite `0..1` with min <= max. Apply inversion next. Alpha-kind masks
then round `value*255` to the nearest byte (half upward); explicit inversion/ranges
can therefore select transparent pixels. Other kinds exclude pixels below integer
`alpha_min` (default 1) and, by default, multiply coverage by source alpha/255 before
rounding. `preserve_alpha=false` disables that multiplication; it is a strict boolean.
Inspecting hidden RGB requires explicit `alpha_min=0`. Default color/luminance masks
preserve transparency and partial coverage. The source RGBA bytes never change.

| Mask call | Result and rules |
|---|---|
| `mask:info()` | `{width,height,bytes,format="a8",origin="top_left"}`; retained bytes equals width*height |
| `mask:pixel(x,y)` | one integer coverage byte `0..255`; exact integer coordinates inside the mask |
| `mask:row(y [,x [,count]])` | packed coverage bytes left-to-right; x defaults 0, count defaults remaining width; empty/out-of-bounds rows fail |
| `mask:read{x=0,y=0,width=...,height=...}` | packed row-major coverage bytes; dimensions default to the remainder; nonempty and inside the mask; at most 1 MiB |
| `mask:bounds([threshold])` | exact `{x,y,width,height,count}` enclosing all pixels with coverage >= threshold; threshold integer `1..255`, default 1; nil when none |
| `mask:regions([options])` | dense sequence of connected-component `{x,y,width,height,count}` summaries; bounded exact ROI scan described below |
| `mask:colorize{r,g,b,a}` | native immutable RGBA image; straight RGB preserved, alpha rounded from coverage*colorAlpha/255; described below |
| `mask:close()` | idempotent immediate release; no result |

Masks share the 64-creation limit, dimensional/pixel ceilings, job scope and
256 MiB plugin / 512 MiB global native accounting with images. Retained storage is one
byte per pixel, without a duplicated RGBA image. Closing the source does not close its
mask. Packed query buffers are separately reserved, capped at 1 MiB and copied to a
Lua binary string. Mask generation and bounds check cancellation every 4,096 pixels;
histograms every 4,096 examined samples. Results expire eagerly on job completion,
failure, cancellation or lifecycle cleanup even when retained in Lua globals. They
cannot be serialized as consent/resource tokens in presets. Retained native handle
metadata is charged separately as described under Geometry span masks.

`mask:regions{threshold=1,connectivity=4,max_regions=4096,x=0,y=0,width=...,height=...}`
accepts only these keys. The ROI defaults to the remainder of the mask and follows
the same nonempty integer bounds as mask reads. Coverage >= threshold (integer 1..255)
is foreground. Connectivity is 4 (edge neighbors) or 8 (also diagonals). Components
are reported in first-foreground-encounter row-major order, with absolute image-space
bounds and exact pixel counts, without alpha weighting. ROI edges cut connectivity;
no pixels outside the ROI participate. Empty foreground returns an empty sequence.

`max_regions` is `1..plum.images.max_regions` (4,096). Exceeding this count fails with
`budget` and returns no partial list. A conservative native workspace reservation
precedes allocation: a four-byte queue entry per ROI pixel, one visited bit per pixel,
and space for max_regions compact result records. This may refuse an otherwise small
query when other resources consume the pool. Scanning/flood fill checks cancellation
at least every 4,096 outer pixels/queue visits; output construction also checks. The
source mask remains unchanged on success, failure or cancellation. No full label map,
point cloud or per-pixel Lua table is returned. Invalid options/ranges produce runtime,
workspace/count exhaustion budget, allocation failure memory; normal expiry/cancel
categories are preserved. This is connected-region analysis, without contour tracing.

Bad types/options/coordinates/colors/ranges and histogram/palette argument bounds
produce `runtime`; cumulative resource/native/query-byte quota exhaustion produces
`budget`; explicit native allocation failure produces `memory`. Expired masks produce
`expired`, and cancellation/timeouts retain their categories. No Lua runs on paint or
audio threads. No project mutation or Undo boundary is created by these services.

```lua
plum.require_api(0, 2)
local image = plum.images.from_rgba(2,1,string.char(255,0,0,255, 0,255,0,128))
local mask = image:mask{kind="luminance", min=0.7}
assert(mask:row(0) == string.char(0,128))
local bounds = mask:bounds()
assert(bounds.x==1 and bounds.width==1 and bounds.count==1)
local regions = mask:regions{connectivity=8,max_regions=16}
assert(#regions==1 and regions[1].count==1)
local histogram = image:histogram{channel="green", bins=2}
assert(histogram.samples==2 and histogram.counts[2]==1)
local h,s,v = plum.images.rgb_to_hsv(0,255,0)
assert(h==120 and s==1 and v==1)
image:close() -- mask remains valid during this job
assert(mask:pixel(1,0)==128)
mask:close()
```

```lua
-- Pure generated pixels; no file authority required:
local image = plum.images.from_rgba(2,1,string.char(255,0,0,255, 0,255,0,128))
assert(image:row(0,1,1) == string.char(0,255,0,128))
local green = image:crop{x=1,width=1,height=1}
local enlarged = green:resize(8,8,"nearest")
local turned = enlarged:transform{rotate=90,flip_x=true}
assert(turned:info().width==8)
assert(turned:pixel(0,0)==0) -- red channel of the green pixel
green:close(); enlarged:close(); turned:close()
image:close()
```

Programmable custom graphs are a mandatory planned UI/math extension tracked as
[GRAPH-01](v1_0_1_api02/CUSTOM_GRAPHS.md). No arbitrary custom-graph support is claimed
by these image primitives; the original 0.1 native `curve` parameter remains supported.

<a id="native-image-composition-api02-development"></a>

## Native image composition (API 0.2)

`plum.images.composite(layers,{width=...,height=...,background={r,g,b,a}?})` combines
0–32existing native RGBA images into one immutable image. Empty layers create only
the authored background, default transparent black. Output dimensions follow the
existing integer1–16384side/16,777,216pixel limits. Each dense layer is a strict plain
`{image=image,x=0,y=0}` descriptor. Integer pixel offsets default0 and must fit signed
32bits. All supplied images must be open and from this job, including offscreen
layers. There is no file/project/UI authority or raw pixel pointer access.

Layer order is bottom to top; the last layer is placed over earlier layers. Source
rectangles are clipped to the output with fast offscreen exclusion. Sources retain
their original dimensions/pixels; this operation does not resize, resample or mutate
them. Transform sources separately with the existing affine/image services when
needed. Background is a strict plain four-byte RGBA array.

The blend is straight-alpha source-over in **stored byte color space**, with no
linear-light/profile conversion. For source alpha `sa` and destination alpha `da`
(integer bytes), define `A=sa*255+da*(255-sa)`. Output alpha is `floor((A+127)/255)`;
each RGB byte is nearest integer (half up) of
`(sourceRGB*sa*255+destinationRGB*da*(255-sa))/A`. Zero-alpha source pixels preserve
all destination bytes, including hidden RGB; an opaque source replaces all bytes.
This defines the transparent/rounding policy independently of GPU blend rounding.
Result `info().format` is `rgba`; reads, color analysis, native note plans and the UI
image adapter work as before. Close sources and result independently.

`max_composite_layers=32` and `max_composite_work=67,108,864` are discoverable. Work
includes output initialization plus every clipped source pixel visit, even repeated
images. Larger work fails before output allocation. The host reserves native source
descriptors/output/512byte handle metadata, borrows immutable source buffers without
copying them, and checks cancellation during descriptor parsing, every output chunk
and at most4096pixel visits inside a row. No partial image is published on quota,
allocation, VM, cancellation or scope failure. Descriptor/native/work/resource limits
retain the existing error categories. The shared64image+mask creation limit applies
only once to the composite result; internal source snapshots create no Lua handles.

```lua
local red=plum.images.composite({},
  {width=2,height=1,background={255,0,0,255}})
local blue=plum.images.composite({},
  {width=1,height=1,background={0,0,255,128}})
local image=plum.images.composite({{image=red},{image=blue,x=1}},
  {width=2,height=1})
assert(image:row(0)==string.char(255,0,0,255,127,0,128,255))
red:close();blue:close()
assert(image:pixel(1,0)==127)
image:close()
```

This lets a Lua author composite bounded colorized glyph layers into one native UI
image, preserving the16UI-image creation budget. It adds no automatic artistic
algorithm, new font decoder, per-note RGBA or extra UI/audio-thread Lua execution.

<a id="native-mask-colorization-api02-development"></a>

## Native mask colorization (API 0.2)

`mask:colorize({red,green,blue,alpha})` creates a normal immutable job-scoped RGBA
image directly from A8 coverage. The color is a strict plain dense four-element
array of integer bytes0–255; no floats, metatables, holes or extra fields. For each
coverage byte `c`, RGB is the exact authored RGB, including under zero alpha, and
output alpha is `floor((c*alpha+127)/255)`. This is straight alpha, not premultiplication.
The mask remains unchanged. `image:info().format` is `rgba`; all existing image read,
transform/cache, native note-plan and UI image operations apply. Source mask and
result own independent native buffers and may be closed independently within a job.

No file/project/font authority or Lua pixel string/table is needed. Mask/image scope
and the shared64creations/job limit apply, including closed resources. Native output
bytes and a512byte image-handle allowance are reserved before colorization allocates;
allocation failure returns no partial image. Native loops check cancellation every
4096pixels and before publication. The existing side/pixel/native/VM bounds apply.
The UI image adapter separately converts straight colors for its native renderer;
colorization itself does not alter Track/Subchannel colors or create notes.

All RGBA image handles now separately account512native bytes for the native cell and
weak job reference until Lua collection, matching A8/Font resource lifetimes. Close/
job completion/failure release pixels eagerly, while a retained closed/expired handle
stays charged. Cache hits share pixel storage and add only their own512byte handle;
`info().bytes` reports pixel bytes. Native note plans retain their own existing bounded
snapshot/job metadata independently. This is an accounting change to API0.2 resources;
API0.1 behavior and image pixel/cache identities are preserved.

```lua
local mask=plum.images.mask_from_spans(
  {row={0},first={0},last={1}}, {width=2,height=1})
local image=mask:colorize{20,80,160,128}
assert(image:row(0)==string.char(20,80,160,128,20,80,160,0))
mask:close()
assert(image:pixel(0,0)==20)
-- An authorized user-invoked panel may pass image to plum.ui.image and then
-- place that native resource in its canvas. No raw GPU or UI-thread Lua runs.
image:close()
```

A plugin may obtain original color glyph layers, flatten/jointly fill each layer's
contours into A8 and colorize using that layer's palette bytes. The author chooses
layer presentation, transformations and supported musical color routing. This
primitive does not itself invent a text/art effect or execute other font formats.

<a id="geometry-span-masks-api02-development"></a>

## Geometry span masks (API 0.2)

`plum.images.mask_from_spans(spans, {width=...,height=...})` creates the same
immutable A8 mask type directly from geometry cell coverage. It needs no file,
font or project permission. The plugin authors the shape, glyph flattening, grid,
axis interpretation and musical mapping. No RGBA intermediate or per-pixel Lua
array is required; this is binary coverage, not an antialiased font rasterizer.

Options accept only required integer `width` and `height`, each `1..16384`, with
at most16,777,216pixels total. Span input is the existing strict plain geometry
format: equal dense `row`, `first`, `last` integer columns, at most65,536spans
(`plum.geometry.max_raster_spans`). Coordinates are zero-based; `last` is exclusive.
Every nonempty span must lie entirely inside the supplied dimensions. There is no
implicit clipping or origin conversion. Each covered cell becomes255, every other
cell0. Unsorted, adjacent and overlapping spans are unioned without mutating input.
Empty columns produce a transparent mask. Optional `count` must match input length;
optional `cell_count` must match canonical union coverage, not a sum with overlaps.
Metatables, unknown fields, holes, fractional/NaN/infinite/out-of-range values fail.
Geometry grid dimensions and mask dimensions are independent explicit inputs: use
the same dimensions to preserve the intended coordinate mapping.

The service reuses the native Boolean-raster validator/normalizer and existing A8
mask, sampler, resource limits and eager cleanup. Pixel/native span workspace are
reserved before allocating their buffers; output is published only after validation
and filling succeed. Cancellation is checked during input, before/after the bounded
65,536span sort, every64span validation/coalescing steps, every64KiB zero fill and
before each at-most16KiB coverage fill. Sorting is cooperatively checked, not
forcibly preempted. No full Lua pixel table is built. Reads remain capped at1MiB.
Invalid descriptors produce `runtime` (unequal/oversize columns and excessive pixel
counts use the existing `budget` category); allocation preflight uses `budget`,
fallible native allocation uses `memory`, and cancellation/deadline retain their
normal categories. Failure does not publish a partial mask or edit any notes.

All A8 and RGBA images separately account512native bytes per retained handle/cell.
`info().bytes` remains pixel bytes only. Close/end-of-job immediately release pixels;
a closed/expired handle retained in Lua remains charged until garbage collected.
Job references are weak. The shared64image/mask creations per job includes closed
handles; creating a sampler separately charges its existing prefix index/metadata.

```lua
plum.require_api(0,2)
local g=plum.geometry
local grid={columns=8,rows=4}
local outer=g.rasterize_polygon({x={0,8,8,0},y={0,0,4,4}},grid)
local hole=g.rasterize_polygon({x={2,6,6,2},y={1,1,3,3}},grid)
local spans=g.raster_boolean(outer,hole,grid,"difference")
local mask=plum.images.mask_from_spans(spans,{width=8,height=4})
assert(mask:bounds().count==24 and mask:pixel(3,1)==0)
local sampler=mask:note_sampler{
  start_tick="9007199254740993",end_tick="9007199254741017",
  first_key=60,last_key=63,sample="start"
}
mask:close() -- sampler owns its bounded snapshot independently
local coverage,matched=sampler:sample(9007199254741002,3,62)
assert(coverage==0 and not matched)
-- Pass sampler to tx:mask_notes(query,sampler,options) for normal private
-- Preview/Apply/Cancel and native Undo; only that edit needs note authority.
sampler:close()
```

Font glyph contours can be mapped and flattened through `plum.geometry`, filled
jointly with `rasterize_region` to preserve counters, then passed to this service.
It does not add unsupported font formats, color emoji or automatic artistic effects.

<a id="bounded-image-note-plans-api02-development"></a>

## Bounded image-note plans (API 0.2)

`image:note_plan(options)` creates an opaque job-scoped plan using the same native
palette classification and time-run segmentation as the built-in Image to MIDI tool.
It performs pure computation, with no project mutation or new permission for procedural
images. Loading a selected file still requires file.read_user_choice. The Lua author
chooses the palette, transformations/resizing, note interpretation and later editing
operation. The host creates no advanced-function catalog or unsupported note RGBA.

Options are a strict plain table:

| Field | Meaning |
| --- | --- |
| `palette` | Required dense array of1–1024plain RGB byte triplets. Nearest squared RGB distance wins; ties use the first entry. |
| `ignored` | Optional dense boolean array exactly matching palette length. The nearest entry is chosen first; if ignored, that cell produces no note. |
| `alpha_min=128` | Strict integer0–255; pixels below this threshold are excluded before nearest-color lookup. |
| `orientation="piano_roll"` | Rows are pitch lines, x increases through time, top row has the highest key. `"upstream"` maps columns to increasing keys and bottom-to-top rows through time. |
| `keys=128`, `first_key=0` | Explicit128/256key space and lowest mapped key. Every pitch line must fit; resize using the existing Image API first. This pure computation does not infer a live project. |
| `pixel_notes=false` | Strict boolean; true yields one note per included cell. Otherwise neighboring equal palette indices form runs. |
| `max_length=0`, `split="note"` | Maximum run length in cells (0means unlimited, at most262144). `note` splits from each run start; `image` splits on global time-cell boundaries. |
| `velocity=100` | Strict integer1–127. Later author code can replace velocity or other supported musical fields. |
| `start_tick=0` | Exact nonnegative tick origin in the author's chosen domain, such as the writer target's local ticks. |
| `tick_numerator=5`, `tick_denominator=1` | Exact integer/decimal tick values. Denominator must be positive and numerator at least denominator, ensuring every cell can have a positive native integer gate. |

Boundary tick is `start_tick + floor(cell*tick_numerator/tick_denominator)` using
checked native integer arithmetic, not repeated floating-point addition. Gate is the
difference between run-end and run-start boundaries. The complete time domain must
fit0–2^63−1, including transparent tail cells. No automatic resize or zero-gate rounding
is applied. To represent a coarser time grid, resize the image before planning.

```lua
local image=plum.images.from_rgba(4,1,string.char(
  255,0,0,255, 255,0,0,255, 0,0,255,255, 0,0,0,0))
local plan=image:note_plan{
  palette={{255,0,0},{0,0,255}},first_key=60,start_tick=120,
  tick_numerator=3,tick_denominator=2
}
image:close() -- plan owns independent native indices, not a source/file lease
assert(plan:info().note_count==2 and plan:info().end_tick==126)
local page=plan:next(1)
assert(page.start[1]==120 and page.gate[1]==3 and page.palette[1]==1)
plan:reset()
assert(#plan:next().start==2)
assert(plan:close() and not plan:close())
```

| Plan method | Result |
| --- | --- |
| `info()` | Fresh `width,height,keys,first_key,orientation,note_count,read,remaining,done,palette_count,palette_base=1,order="line_time",start_tick,end_tick,tick_numerator,tick_denominator,index_bytes,note_record_bytes`. |
| `palette()` | Independent dense RGB triplets in authored order. These are palette metadata, not persisted per-note colors. |
| `next([count=4096])` | Packed equal-length columns `start,gate,key,velocity,palette`; count is integer1–4096. Palette indices are1-based. After the last note, returns empty columns. |
| `reset()` | Rewinds the cursor/read count over the same immutable native data; performs no new indexing or file access. |
| `close()` | True once while data exists, false afterward; closes data immediately, including expired resources for cleanup. |

Read order is pitch-line then time, matching native run traversal. It is deterministic
and independent of page size; it is not globally sorted by tick. Existing note writers
sort their completed buffers once. Reads advance only after native processing and Lua
serialization succeed; failed/canceled/native-budget/VM-memory reads return no partial
page or progress. Source Image closure does not close its plan. Plans, reset and reads
expire eagerly at job end/error/cancel even if userdata remains in a global. They cannot
be serialized as preset/resource tokens. Keep plain palettes and mapping parameters.

To insert through the existing transaction-owned writer, `project.read` and notes.edit
are required. This complete example maps both authored palette entries to the target's
existing native main color. The native ingestion API below also supports explicit
Subchannel routing. For author-edited packed pages, remove the palette interpretation column before appending supported note columns:

```lua
local project=assert(plum.project.current())
local keys=project:key_range()
local image=plum.images.from_rgba(4,1,string.char(
  255,0,0,255, 255,0,0,255, 0,0,255,255, 0,0,0,0))
local plan=image:note_plan{
  palette={{255,0,0},{0,0,255}},keys=keys,first_key=keys==256 and 200 or 60,
  start_tick=120,tick_numerator=3,tick_denominator=2
}
image:close()
local tx=plum.transaction{label="Authored image-note plan"}
local writer=tx:note_writer(project:track(1))
while true do
  local page=plan:next()
  if #page.start==0 then break end
  page.palette=nil -- author deliberately uses the target's existing color
  writer:append(page)
end
writer:finish()
plan:close()
tx:commit()
```

`writer:append_plan(plan [, palette_map])` consumes **all remaining** notes directly
in native code and returns the number staged. Prior `next` reads are respected;
`reset` explicitly permits replay. Omit the map to use native main Subchannel ID0
and the writer's captured track default MIDI channel. A supplied map must be a plain,
dense array exactly matching `palette_count`, with each entry either a Subchannel
handle from the writer's hosting track or `false` to skip that palette entry. Numeric
IDs, foreign/stale handles, holes, extra fields and metatables are rejected. Each
mapped handle supplies its native Subchannel ID **and MIDI channel**. Display colors
remain those of the native Track/Subchannel system; this operation does not assign
an arbitrary RGB value to an individual note or change native color settings.

```lua
local project=assert(plum.project.current())
local track=assert(project:track(1))
local image=plum.images.from_rgba(3,1,string.char(
  255,0,0,255, 0,0,255,255, 0,0,0,0))
local plan=image:note_plan{
  palette={{255,0,0},{0,0,255}},keys=project:key_range(),first_key=60,
  start_tick=120,tick_numerator=3,tick_denominator=2
}
image:close()
local tx=plum.transaction{label="Native image palette routing"}
local first=tx:add_subchannel(track,{name="Authored palette 1",channel=0})
local second=tx:add_subchannel(track,{name="Authored palette 2",channel=1})
local writer=tx:note_writer(track)
assert(writer:append_plan(plan,{first,second})==2)
assert(plan:info().done and track:notes():count()==0)
writer:finish()
plan:close()
tx:commit()
```

This example additionally needs tracks.edit to create Subchannels; using existing
handles needs only the writer's project.read/notes.edit grants. Tracks use song ticks;
owned Clip content uses local ticks and its host's Subchannels, including linked
placements. No implicit time shift, resizing or target selection occurs. Skipped
entries consume input but do not count against generated-note quota. All accepted
notes still count even if the writer or transaction is later canceled, like append.

The plan cursor advances only after the complete native stage is accepted. Invalid
mapping/key/target/revision, native allocation refusal or cancellation leaves its
read progress unchanged and poisons the writer, releasing its private buffer. Cancel
that failed writer or its transaction. A later finish/transaction failure discards
musical changes but does not rewind a previously successful append_plan; reset is
explicit. The shared writer performs final group/target validation, stable ordering,
native planner preflight and one transaction Undo/Redo boundary. Limits and expensive
operation confirmation are unchanged. No per-note Lua tables are allocated by native
ingestion; native indices, writer records, sorting and history remain budgeted.

The existing standard-MIDI out-of-range policy remains unchanged; keys above127 need
an explicit supported export choice such as Experimental256-Key MIDI.
Normal writer/project locks, local target coordinates, output confirmation budgets,
Preview/Apply/Cancel and Undo rules remain authoritative. Planning alone changes no
musical content or history. `note_record_bytes` is count times native NoteRecord size;
it excludes writer ordering/insertion/history/selection workspace and is not an RSS
measurement or promise that the project can accept that many notes within its budget.

Limits:16parsed plan-construction attempts/job,16,777,216cumulative indexed cells and
268,435,456conservative cell×palette comparisons/job. Closing/reset does not restore
construction quotas; failed attempts after work admission retain those work charges.
Public constants are plum.images.max_note_plans/max_note_plan_batch/max_note_plan_work;
the cumulative cell cap equals plum.images.max_pixels. Native preflight reserves8192
metadata bytes per plan before parsing, then4bytes per indexed cell before allocation.
Read scratch is reserved for at most4096mapped rows; Lua tables count toward the VM cap.
Closed/expired userdata keeps its metadata charge until GC; active data owns its index
charge until close/end-run. All normal plugin/global image/native/VM budgets still apply.

Indexing, counting and long transparent/uniform run scans check cancellation every4096
cell inspections. Packed reads never allocate a per-note Lua object for the whole plan.
These are native worker/host APIs; no renderer/audio callback executes Lua. Direct native
plan-to-writer color mapping and broader image-conditioned editing are subsequent
ART01/ART02 work, not claims of this initial plan API.

<a id="image-mask-note-sampling-api02-development"></a>

## Image-mask note sampling (API 0.2)

`mask:note_sampler(options)` indexes an existing compact A8 mask into an opaque,
job-scoped native sampler. Compose the existing `image:mask` alpha, color-key,
brightness or luminance services first. No additional permission is needed for
procedural data. User-selected source files retain their existing consent rules.
Sampling computes coverage; it does not modify notes or select a project target.

Options are a strict plain table:

| Option | Meaning |
| --- | --- |
| `start_tick=0`, `end_tick` | Required exclusive end, greater than start. Exact integer/decimal ticks in0–2^63−1. These are the author's chosen song or Clip-local coordinates. |
| `first_key=0`, `last_key=127` | Inclusive authored key domain0–255, first≤last; supports both key modes without inferring a live project. Out-of-domain pitches are unmatched. |
| `orientation="piano_roll"` | Image x increases through time; top image row corresponds to highest pitch. `"upstream"`: image columns increase through pitch, bottom-to-top rows through time. |
| `sample="start"` | `start`, `center`, `any`, `all` or `average`; definitions below. |
| `threshold=128` | Strict A8 byte0–255, used for native match decisions. Outside the mapped domain is always unmatched, including threshold0. |

Pitch is sampled at the center of its row in the inclusive key domain. This scales
arbitrary mask dimensions to the declared pitch range; it does not create extra note
rows. Time boundaries use exact native integer arithmetic relative to start_tick,
including origins above2^53. There is no conversion of absolute ticks to floating point.

`start` samples the cell containing the note start; `center` samples at
`start + floor(gate/2)`. The point must lie in `[start_tick,end_tick)`. Body policies
clip the note's half-open interval to that domain, then sample every image-time cell
intersected by the clipped interval. A zero gate uses a one-tick interval for body
policies and the start point for point policies. `any` matches when at least one
sampled cell is≥threshold; `all` requires every sampled cell; `average` compares the
arithmetic mean to threshold. Coverage is the mean A8 byte value (0–255) for all
policies; body means count intersected cells equally, without partial-cell weighting.
A wholly outside note has coverage0 and matched=false. No extrapolation occurs.

| Method | Result |
| --- | --- |
| `sampler:sample(start, gate, key)` | Two values: numeric `coverage`, boolean `matched`. Inputs use exact tick rules and strict key0–255. |
| `sampler:sample_batch{start={},gate={},key={}}` | Equal-length plain dense arrays, at most4096notes; returns packed `{coverage={},matched={}}`. No extra fields or metatables. Empty input is valid. Failure returns no partial batch. |
| `sampler:count_notes(query)` | Exact count of existing query notes matched by this sampler; needs project.read and a live Track/owned-content query. Uses the existing native overlap index, preserving every query filter and selection. |
| `sampler:info()` | Fresh `width,height,time_pixels,pitch_pixels,start_tick,end_tick,first_key,last_key,orientation,sample,threshold,index_bytes`. |
| `sampler:close()` | Releases index data immediately; true once, false afterward. |
| `plum.images.max_note_samplers`, `max_note_sample_batch` | 16construction attempts/job and4096packed samples/call. |

```lua
local image=plum.images.from_rgba(4,1,string.char(
  0,0,0,0, 0,0,0,64, 0,0,0,128, 0,0,0,255))
local mask=image:mask{kind="alpha"}
local sampler=mask:note_sampler{
  start_tick=120,end_tick=132,first_key=60,last_key=60,
  sample="any",threshold=128
}
image:close()
mask:close() -- sampler owns an independent native index
local coverage,matched=sampler:sample(120,12,60)
assert(coverage==111.75 and matched)
local batch=sampler:sample_batch{
  start={120,123,132},gate={12,3,1},key={60,60,60}
}
assert(batch.coverage[2]==64 and not batch.matched[2])
assert(batch.coverage[3]==0 and not batch.matched[3])
sampler:close()
```

The host builds prefix sums and threshold counts once, using native preflight,
fallible allocation and cancellation checks every4096cells. Each later note costs
constant native work, independent of gate length or image width. Index reservation
is8bytes per cell plus8bytes per pitch line; metadata is charged separately. The
shared native budget may refuse a large index. At most16,777,216cells may be indexed
cumulatively per job; close does not refund attempts or work. Masks may close after
construction. Samplers expire eagerly at job end/error/cancel; retaining a userdata
keeps only bounded metadata until collection. Save plain mapping settings in presets,
not opaque sampler handles. Existing native image conversion/API0.1 remain unchanged.
`count_notes` interprets ticks in the query target's native domain: song ticks for
Track/Subtrack notes, local ticks for owned Clip content, including linked placements.
The ordinary query's start range, keys, velocity, channel, Subchannel, group, muted and
selected filters still apply. Sampling a note body does not change the query's start
range into a body-range filter. Counts include readable locked notes; read access
confers no authority to change them. No note tables or match lists are built in Lua.

The native ranked overlap visitor skips off-range chunks using the existing index;
rank bases are walked once, avoiding a per-match scan of preceding chunks. Cancellation
and deadline checks occur during prefix/chunk walks and every4096scanned candidate
notes, including candidates rejected before visiting. Dense local overlaps still take
work proportional to candidates; this is not a constant-time project query.

```lua
local project=assert(plum.project.current())
local track=assert(project:track(1))
local image=plum.images.from_rgba(1,1,string.char(0,0,0,255))
local mask=image:mask{kind="alpha"}
local sampler=mask:note_sampler{
  start_tick=0,end_tick=960,first_key=60,last_key=72,sample="any"
}
local query=track:notes{keys={60,72}}
local matched=sampler:count_notes(query)
assert(matched>=0 and matched<=query:count())
sampler:close()
mask:close()
image:close()
```

Use the native transaction operations below to act on matched notes; counting and
sampling themselves remain read-only.

<a id="native-mask-conditioned-note-transactions-api02-development"></a>

## Native mask-conditioned note transactions (API 0.2)

`tx:mask_notes(query, sampler, options)` acts on existing query notes matched by a
live note sampler. It needs `project.read` and `notes.edit`, reuses the existing
musical transaction/Preview/Apply/Undo path, and returns the number of **matched
source notes**. It never calls a Lua callback for each note. All existing query
filters and native Track/Subtrack/owned Clip-local coordinates remain authoritative.
The plugin author chooses masks, domains, policies and musical interpretation.

`options` is a strict plain table:

| Field | Meaning |
| --- | --- |
| `action` | Required `"select"`, `"delete"`, `"duplicate"` or `"modify"`. |
| `mode="replace"` | Select only: `replace`, `add`, `remove` or `toggle`. Selection is an explicit invocation operation, unavailable to automatic responses. |
| `changes` | Required for modify, optional for duplicate. A plain scalar note-change table described below; forbidden for select/delete. |

Changes reuse native `shift`, `transpose`, `velocity`, `velocity_add`, `velocity_scale`,
`gate`, `gate_add`, `gate_scale`, `channel`, `subchannel`, `outside`, `muted`,
`off_kind`, `off_velocity` and `group`. Unknown fields, metatables, nested values and
functions are rejected. Scalar text is bounded to64bytes; scales must be finite and
nonnegative. `outside` uses the existing `error`/`clamp`/`drop` policy. `subchannel`
is an existing numeric native ID of the target host; this does not create a color or
Subchannel. Native group and ancestor locks govern both source and destination groups.
Actual project key mode, note encoding and Subchannel ownership are validated before
any change. Unscaled gates and integer gate additions preserve exact integer values,
including above2^53. An explicit floating scale uses native floating rounding. Extreme
modifiers fail or clamp according to their documented policy without integer overflow.

Delete removes matched notes. Duplicate inserts changed copies and preserves sources;
`outside="drop"` skips copies without deleting sources. Modify replaces matched notes
with their changed results, deleting those explicitly dropped. Return count includes
dropped or unchanged matched source notes; it is not necessarily inserted-note count.
Only inserted results count against the ordinary output budget. Deleted results retain
the existing expensive-operation confirmation threshold. Selection uses native ranks;
an empty match with replace clears the target selection, while other modes preserve it.
Selection-only commits have the existing selection behavior and create no musical Undo
step. Musical changes share their transaction's one predictable Undo/Redo boundary.

```lua
local project=assert(plum.project.current())
local track=assert(project:track(1))
local image=plum.images.from_rgba(2,1,string.char(
  0,0,0,255, 0,0,0,0))
local mask=image:mask{kind="alpha"}
local sampler=mask:note_sampler{
  start_tick=0,end_tick=960,first_key=0,last_key=project:key_range()-1,
  sample="any",threshold=128
}
local tx=plum.transaction{label="Authored image-conditioned velocity"}
local matched=tx:mask_notes(track:notes{selected=true},sampler,{
  action="modify",changes={velocity=88}
})
assert(matched>=0)
tx:commit()
sampler:close()
mask:close()
image:close()
```

The host first validates/counts indexed matches, preflights native memory and then
stages exact records/ranks in a second bounded native pass. Reservations include
matched records, touched native chunks, existing selection, insertion, compact history
and pending transaction buffers. The shared256MiB/plugin/512MiB/global native pool
may refuse a dense operation. No giant per-note Lua table is required; packed sampling
and native operations can be composed independently. Uniform change tables preserve
native stable time order. Native overlap/cancellation rules match count_notes.

A parsing, validation, output-budget or allocation error during preprocessing leaves
the private open transaction unchanged. Once the native write phase begins, **any
error or cancellation discards and closes the entire open transaction**, including
its earlier edits. This prevents a Lua pcall from committing an erased half of a
replacement. A script that catches such an error must open a fresh transaction.
Preview never modifies the live project; canceled or failed processing returns no
partial commit. Standard MIDI wide-key export and native note-color rules are unchanged.

<a id="boolean-raster-composition-api02-development"></a>

## Boolean raster composition (API 0.2)

`plum.geometry.raster_boolean(a, b, grid, operation)` combines existing raster row
spans as discrete cell sets. Operations are required text `"union"`, `"intersection"`,
`"difference"` (A without B) or `"xor"` (symmetric difference). Use the same grid and
packed representation as rasterize_polygon/polyline/region. No notes are created by
this pure mathematical service and no additional capability is needed.

Both inputs are plain tables with equal-length dense integer columns `row`, `first`
and `last`. Rows are0-based, first is inclusive and last exclusive; spans must have
positive length and lie inside the supplied grid. Empty columns are valid. Optional
`count` must equal the input column length. Optional `cell_count` must equal unique
cell coverage after normalization. Unknown fields, holes, metatables, floats, nonfinite
values and out-of-grid coordinates are errors. Inputs are never modified.

Overlapping, adjacent and unordered input spans are sorted and merged using the same
native canonicalization as existing raster outputs. Result is `{row,first,last,count,
cell_count}` with sorted rows, disjoint nonadjacent spans and exact integer counts.
Each input has at most65,536spans (`max_raster_boolean_input_spans=131072` combined).
Grid limits remain65,536columns/4,096rows. `max_spans` defaults to8192, at most65,536;
for Boolean results this cap applies **after** adjacent output intervals coalesce.
The existing rasterization entrypoints retain their documented pre-union limits.

The grid's origin/cell settings are validated but these operations combine integer
cell indices. They do not align two rasters originally built with different origins,
scales or axes, and are not continuous vector-path clipping. The author must produce
both inputs in the same coordinate grid. Native preflight covers raw inputs and bounded
output before allocation; sort phases are bounded to65,536spans/input, with checks
before/after sorts and every64parse/merge/sweep/output iterations. Error, cancellation,
cap or native/VM memory refusal returns no partial result. Large cell regions stay
compact as row intervals rather than millions of Lua objects or interactive controls.

```lua
local project=assert(plum.project.current())
local g=plum.geometry
local grid={columns=8,rows=8,max_spans=64}
local outer=g.rasterize_polygon({x={0,8,8,0},y={0,0,8,8}},grid)
local hole=g.rasterize_polygon({x={2,6,6,2},y={2,2,6,6}},grid)
local cells=g.raster_boolean(outer,hole,grid,"difference")
assert(cells.cell_count==48 and cells.count==12)
-- The author's musical interpretation: one contiguous gate per row interval.
local first_key=project:key_range()==256 and 200 or 60
local tx=plum.transaction{label="Authored Boolean cell artwork"}
local writer=tx:note_writer(project:track(1))
local batch={start={},gate={},key={},velocity={}}
for i=1,cells.count do
  batch.start[i]=plum.ticks.add(120,plum.ticks.mul(cells.first[i],30))
  batch.gate[i]=plum.ticks.mul(cells.last[i]-cells.first[i],30)
  batch.key[i]=first_key+cells.row[i]
  batch.velocity[i]=100
end
writer:append(batch)
writer:finish()
tx:commit()
```

This example additionally needs the ordinary project.read/notes.edit grants. Native
project keys, Subchannels, locks, output confirmation, Preview/Apply and Undo/Redo
continue to govern the writer. The author can instead map spans into masks, selection
regions, graph datasets or other supported geometry; the host invents no art function.
Keys above127 retain the existing explicit Experimental256-Key MIDI export requirement.

<a id="thick-stroke-note-cell-rasterization-api02-development"></a>

## Thick-stroke note-cell rasterization (API 0.2)

`plum.geometry.rasterize_stroke(points, grid, options)` rasterizes an authored open
or closed path with a configurable thickness. It reuses the existing native Draw
geometry's stroke parts, joins/caps, row-center scanning, Pen quantization and canonical
row-span output. It creates no MIDI notes by itself and needs no extra capability.
The original thin rasterize_polyline and API0.1 drawing/curve editor remain supported.

`points` and `grid` use the existing geometry/raster descriptors. Options are strict:

| Option | Meaning |
| --- | --- |
| `width_cells` | Required finite positive stroke width in normalized grid cells. Each cell is one unit along both normalized axes; grid cell_width/cell_height determine its musical/world footprint. |
| `closed=false` | Strict boolean, closes the last point to the first. |
| `join="round"` | `round`, `bevel` or `miter`. Native miter extensions beyond4half-widths fall back to bevel; `stroke_miter_limit=4` exposes that existing limit. |
| `cap="round"` | Open ends: `round`, `butt` or `square` (extends by half-width). Caps are irrelevant for nondegenerate closed strokes; singleton geometry follows the native cap rule. |

Consecutive equal points and repeated closing endpoints follow native cleanup. A
single point yields a round/square footprint or an empty butt stroke. Inputs are not
modified. Output is the same `{row,first,last,count,cell_count}` cell representation,
clipped to the grid, so it composes with raster_boolean and native note writers.
The existing nearest-grid rounding and at-least-one-column thin-interval fallback
remain authoritative; the path's coordinates and musical mapping belong to the author.

Limits:1–4096points, existing grid dimensions/span cap, and conservative
`(2*input_points+2)*rows <= 4194304` part-row work. Width and normalized coordinates
must remain finite within their documented magnitude bounds. Half-width must be at
least8machine-epsilon units at the largest normalized input coordinate magnitude
(minimum magnitude1); smaller unrepresentable widths fail explicitly. Use local offsets
or rescale when precision is insufficient. Native preflight covers contours, stroke
parts/edges/order, active row buffers/sort workspace and output before building. Empty
fill/band descriptors avoid native boundary-pair extraction. Native build/row phases
are independently bounded; cancellation checks occur before/after build, during point
conversion, each row (including empty rows), interval output and canonicalization.
Failure publishes no partial spans and releases temporary native storage. The existing
max_spans-before-union rule applies, including for strokes. Native and VM quotas remain.

```lua
local project=assert(plum.project.current())
local g=plum.geometry
local cells=g.rasterize_stroke(
  {x={2,8,14},y={3,12,3}},
  {columns=16,rows=16,max_spans=64},
  {width_cells=3,join="round",cap="round"})
assert(cells.count>0 and cells.cell_count>0)
-- The author maps path rows into pitch and row intervals into note gates.
local first_key=project:key_range()==256 and 200 or 60
local tx=plum.transaction{label="Authored thick-stroke artwork"}
local writer=tx:note_writer(project:track(1))
local batch={start={},gate={},key={},velocity={}}
for i=1,cells.count do
  batch.start[i]=plum.ticks.add(120,plum.ticks.mul(cells.first[i],30))
  batch.gate[i]=plum.ticks.mul(cells.last[i]-cells.first[i],30)
  batch.key[i]=first_key+cells.row[i]
  batch.velocity[i]=100
end
writer:append(batch)
writer:finish()
tx:commit()
```

The example additionally needs project.read/notes.edit, and all normal key/Subchannel/
lock/output/Preview/Apply/Undo rules apply. Keys above127 retain the explicit supported
Experimental256-Key export requirement. No artistic brush or advanced function is
created automatically; authors combine this geometry with strokes, images, graph data,
glyph paths and their own mathematical interpretation.

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

`print(...)` joins values with tabs, with at most **16 KiB of UTF-8 per call**,
**1,000 calls** and **1 MiB of encoded output per worker job**. The byte allowance
includes one newline byte per call; empty calls still count. Invalid UTF-8 in direct
string arguments uses replacement characters, which also count toward the limit.
An oversized line is rejected without a partial line. Exhaustion raises **budget**;
an uncaught error discards private musical transactions through the normal failure
path. The worker drains output after each job and remains usable for the next one.

The retained plugin log keeps at most **1,000 lines / 1 MiB of text**, evicting oldest
lines. Each retained line is limited to 16 KiB; a single host log message is clipped
to 64 KiB before bounded line processing. Clipping preserves UTF-8 boundaries and
adds ` [truncated]`. These are text limits; bounded line/queue metadata is additional.
`plum.progress` retains at most 200 Unicode scalars of its message. Notifications
retain at most 300 non-control scalars each and five notices per job. Long progress
or notification strings are read from Lua without first copying the full string.

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

API0.2 adds `s:note_color([velocity])` → `{r,g,b}` (named integer byte fields).
Velocity is a Lua integer1–127, default100. It reads the native unselected note-fill
color from the run's captured application note palette (Random by Seed, Manual or
Rainbow16), theme and velocity. The returned table is independent. It does not read
Track Properties' Onion tint, selection/ghost shading, nor store per-note RGB/RGBA.
Existing `t:color()`/Clip colors and all API0.1 methods are unchanged. Requires the
same `project.read` grant and valid handle; no palette mutation or UI callback.

```lua
local subs = ctx.owner_track:subchannels()
local palette, mapping = {}, {}
for i=1,math.min(#subs,16) do
  local c=subs[i]:note_color(ctx.params.velocity)
  palette[i]={c.r,c.g,c.b}; mapping[i]=subs[i]
end
-- The author chooses quantization/options. The host preserves native note colors.
local plan=image:note_plan{palette=palette,keys=ctx.keys,
  velocity=ctx.params.velocity,tick_numerator=30}
local writer=ctx.tx:note_writer(ctx.track)
writer:append_plan(plan,mapping); writer:finish(); plan:close()
```


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

Columnar inserts read fields directly without creating a temporary Lua note table per
row. Validation, defaults, exact tick handling and stable equal-start ordering match
the row form. This preserves the API 0.1 signature and its existing accepted inputs.

### Optional packed metadata (API 0.2)

Default `batches()` and `each()` still return the original eight API 0.1 fields.
Opt in with `q:batches{size=4096, columns={...}}` or `q:each{columns={...}}`:

| Additional column | Value |
|---|---|
| `muted` | Boolean native mute flag; muted notes remain in LUP, do not play/export |
| `off_kind` | `"note_off"` or `"note_on_zero"`, preserving native MIDI termination spelling |
| `off_velocity` | 0–127; always zero for `note_on_zero` |
| `group` | Project-wide native group ID; 0 means ungrouped |
| `locked` | Boolean from the current group's native ancestor locks when the batch is read; a hint, every write revalidates |
| `track_id` | Host track ID; for owned content, the track currently supplying its Subchannel context |
| `content_id` | Owned content ID, or 0 for track notes |
| `host_kind` | `"track"` or `"clip_content"` |

Notes/selected fields use the iterator's immutable note snapshot. Ownership comes from
its resolved host; `locked` uses the current group table, so it may change between
batches. Use `t:notes{group=id, muted=true}` to filter; either field is optional.
No note IDs, RGBA colors or separate per-note metadata allocations are introduced.

Row/column `insert_notes`, writer append, native `modify_notes`/`duplicate_notes`
changes and callback result tables accept optional `muted`, `off_kind`, `off_velocity`,
`group`. Defaults on insert are false, `note_off`, 0, 0; omitted modification fields
preserve native values. Selecting `note_on_zero` clears a previous off velocity;
explicitly combining it with nonzero `off_velocity` is an error. Group IDs must exist;
locked groups/ancestors refuse inserting members or editing/removing member notes,
including changing a member to group 0. Note selection still uses the existing selection
transaction methods. Query owner/locked/index fields are read-only, not insert fields.
API 0.1 callback input tables keep their original fields; opt-in queries expose metadata.
New optional metadata/filter fields use raw lookups: supply them explicitly; they do
not invoke a legacy table's `__index` handler. Original API 0.1 getter behavior for
its existing fields is unchanged.
Native LUP/SMF serialization and export rules remain authoritative.

```lua
local t = plum.project.current():track(1)
local tx = plum.transaction{label = "Packed note properties"}
tx:modify_notes(t:notes{muted=false}, {off_kind="note_off", off_velocity=64})
for n in t:notes():each{columns={"key", "muted", "off_kind", "off_velocity", "group"}} do
  assert(n.off_kind == "note_off" or n.off_kind == "note_on_zero")
end
tx:commit()
```

### Native note groups (API 0.2)

Groups use the existing project-wide native table and packed note group IDs. Reads
need `project.read`; writes need `project.read` and `notes.edit`. Track/Subchannel
organization and colors remain independent. There is no per-note RGBA or new note ID.

| Call | Result |
|---|---|
| `p:group(id)` | Group handle, or nil if the current ID is absent (including 0) |
| `p:groups{offset=0, limit=256}` | Group handles in ID order; offset is zero-based, maximum page 4,096 |
| `g:id()`, `g:name()` | Current ID and name (empty string when unnamed) |
| `g:group_info()` | `{id, parent, name, color, locked, effective_locked, temporary, link}`; absent name/color are nil; parent/link 0 means none |
| `tx:create_group([options])` | New native group handle |
| `tx:edit_group(g, options)` | Edit fields through native history; omitted fields unchanged |
| `tx:link_groups({g1,g2,...}, [linked=true])` | Link at least two distinct groups, using the smallest listed ID as the native link-set ID. `false` unlinks the listed groups; one is sufficient |
| `tx:delete_group(g)` | Delete an unlocked empty leaf; refuses any members or children |

Options are plain tables with `parent`, `name`, `color`, `locked`, `temporary`.
Parent is a group handle; `parent=false` clears it. Name is valid UTF-8 of at most
64 characters/256 bytes; `name=false` clears it. Color is native group outline
`{r=0..255, g=0..255, b=0..255}`; `color=false` restores default. Flags are booleans.
Cycles, excessive native nesting (64), missing parents and invalid values are errors.
Use `tx:modify_notes(query, {group=g:id()})` to add/reassign members; `{group=0}` removes
membership. Unassign members and reparent/remove children explicitly before deletion.
This keeps note values intact and makes each action visible in the same transaction.

Native link sets are saved and understood by existing native group tools. Lua note
queries/edits affect their explicit targets; they do not automatically propagate an
author's algorithm to linked groups. Enumerate the group's `link` peers and implement
that behavior in Lua when desired. Linking changes only the listed groups' links.
Owned Clip content is shared by its placements; editing that content affects every
linked placement, independently of group link sets. Temporary groups are omitted by
native LUP save, and their member notes reopen ungrouped, preserving native behavior.

Published group handles use the project serial and native incarnation ledger; deletion
or Undo of creation permanently expires them even if that ID returns. Handles for
new groups in a private transaction also carry transaction scope, so canceled IDs
cannot revive a handle. Those private handles expire at a later transaction/job;
reacquire with `p:group(id)` after Apply. Handles for already published groups remain
usable across transactions while their native incarnation remains valid. Raw numeric
IDs in note columns are current-table values, not persistent identity tokens.

Locks use the native `GroupTable::locked`: a group or any ancestor can lock a note.
Lua edits refuse locked source notes (delete/modify/duplicate, even changing group to
0) and locked destinations, including streaming finish after a lock changes. Selection
and reads remain allowed. Deleting a Track/subtree, deleting a Subchannel, moving its
notes to another channel, or moving/resizing/slicing/muting/duplicating/deleting/merging
Clip placements refuses affected locked content. A mixed Track deletion checks all
requested roots before changing any. Display names/colors and Track List ordering
remain editable. Group names/colors may change while locked; reparenting, temporary
persistence changes and link changes require an unlocked group. Explicitly clearing a
child's own lock does not bypass a locked ancestor. Unlock first, then change structure;
combining unlock and reparent cannot bypass an existing lock.

There are at most 1,024 group-table edit attempts per job and 4,096 handles per list/link
request; native group count/depth bounds still apply. Group table copies, link buffers,
shared-content scan workspace and retained Preview/operation data use the shared
native quota. Note lock checks visit requested ranks through chunk metadata, not every
intervening note. Structural checks may scan affected stores with cancellation every
4,096 notes. Native validation/planner/history phases have before/after cooperative
checks. Failed validation leaves the musical working copy unchanged; cancellation
following native group apply discards the current open transaction. Preview/Apply,
Undo/Redo and existing serializers remain the transaction authority.

```lua
plum.require_api(0, 2)
local p = assert(plum.project.current())
local t = assert(p:track(1))
local tx = plum.transaction{label="Author's native groups"}
local parent = tx:create_group{name="Artwork", color={r=120,g=40,b=200}}
local child = tx:create_group{name="Stroke", parent=parent}
tx:insert_notes(t, {{start=0,gate=120,key=60,off_velocity=32,group=child:id()}})
tx:edit_group(parent, {locked=true})
assert(child:group_info().effective_locked)
tx:edit_group(parent, {locked=false})
tx:modify_notes(t:notes{group=child:id()}, {off_velocity=48})
local hash = p:content_hash()
tx:commit()
return hash
```

### Streaming note writers (API 0.2)

Needs `project.read` and `notes.edit`. `tx:note_writer(t_or_c)` opens an opaque writer
for a track or an owned clip **content** handle. Use `clip:content()` for a clip.
Track notes use song ticks; owned content uses Clip-local ticks, shared by linked
placements. Writers reuse the native insert planner and musical transaction/Undo path.

| Call | Result |
|---|---|
| `w:append_plan(plan [, palette_map])` | Stages remaining native image-plan notes with explicit same-host Subchannel handles or false; see [image plans](#bounded-image-note-plans-api-02) |
| `w:append(batch)` | Accepts a bounded row list or column table; returns the batch's note count. Notes remain private staging |
| `w:finish()` | Validates, stably orders and inserts all staged notes into the transaction's working copy once; returns total notes |
| `w:cancel()` | Discards an unfinished/failed writer. Idempotent; finished writers require canceling their transaction |
| `w:info()` | `{state, notes, staged_bytes, max_batch}`; states `open`, `busy`, `finished`, `canceled`, `failed`. `notes` is the last accepted count, including after discard; `staged_bytes` is reserved note-buffer capacity, excluding planner/transaction overhead |
| `plum.notes.max_write_batch` | 65,536 notes per append |
| `plum.notes.max_writers` | 32 writer creations per transaction, including canceled/finished writers |

Writers accept `start`, `gate`, `key`, `velocity`, `channel`, `subchannel`, plus
optional packed `muted`, `off_kind`, `off_velocity` and `group` (see below). Start,
gate and key are required. Velocity defaults to 100, channel to the track's captured
default, Subchannel to ID 0. Use `sub:id()`, not the displayed `sub:number()`.
Ticks retain the API 0.1 exact-number rules (0 through `2^63-1`; large exact integers
or decimal strings). Keys must fit 128/256-Key Mode. Numeric/encoding strings may
contain at most 64 bytes. Only `muted` accepts a boolean. Invalid values are errors.

Writer tables and nested tables must be plain, without metatables. Arrays must be
dense, indexed 1 through their length, with no extra fields or holes. Supplied columns
must have equal lengths. No `count`, `index`, `selected`, colors or other fields
are accepted. To stream a query, explicitly project its supported columns. The legacy
`tx:insert_notes` parser remains compatible with API 0.1.

Append does not insert/reindex the target. Native staging grows geometrically, with
preflight for overlapping old/new buffers. Finish orders arbitrary starts stably:
ties retain their original stream order, independent of append batch size. Sorted
streams skip ordering scratch. The host preflights touched chunks, packed note data,
planner copies and compact history workspace, using the shared 256 MiB/plugin and
512 MiB/global native pool. These are conservative reservations, not measured RSS.
The existing per-run output cap also applies: every **accepted** writer note counts
once, even if its writer or transaction is canceled. Finished notes are not counted
twice. Available native memory can limit output below the nominal 50-million-note cap.

Do not edit the writer's target notes, replace its target, or change its track default
channel between opening and finishing. Append/finish revalidate handle incarnation,
host, note revision, key mode and Subchannels; stale targets fail. Writers for independent
targets can finish within one transaction. A query iterator captures a snapshot, so it
can supply a writer for the same target without reading newly inserted notes.

Any failed append/finish after acquiring an open writer drops its staging and marks
it `failed`; explicitly cancel that writer or its transaction. A caught error cannot
later finish a prefix. Commit refuses unfinished, busy, failed or abandoned writers.
An error/cancellation during or after the native finish insertion cancels the **whole
open transaction**, including its earlier edits. Earlier committed transactions remain
subject to the normal run outcome. Uncaught run failure discards all pending commits.

Own parse/copy/ordering loops check cancellation at most every 4,096 records; native
planner/history phases check before and after, without forced mid-phase preemption.
All work runs on the worker. No Lua callbacks are dispatched by the writer. Retained
Lua handles own no note buffers: cancel/job completion eagerly drops staging, and
cross-job methods expire (`cancel` remains harmless cleanup). Pending Preview commits
retain native reservations until applied/dropped; musical history uses the existing
host Undo limits. Each committed transaction is one Undo boundary. Preview changes
only the private working copy; Apply uses the captured project revision.

```lua
plum.require_api(0, 2)
local p = assert(plum.project.current())
local target = assert(p:track(1))
local tx = plum.transaction{label = "Author's streamed notes"}
local writer = tx:note_writer(target)
for first = 0, 4095, 1024 do
  local batch = {start = {}, gate = {}, key = {}}
  for i = 1, 1024 do
    local n = first + i - 1
    batch.start[i], batch.gate[i], batch.key[i] = n * 120, 90, 48 + n % 24
  end
  writer:append(batch) -- Author-defined algorithm; bounded Lua input.
  plum.progress((first + 1024) / 4096, "Staging notes")
end
writer:finish() -- Now visible in tx, still absent from the live project.
local preview_hash = p:content_hash()
tx:commit() -- Host applies through its normal transaction/Undo path.
return preview_hash
```

For a snapshot query batch `b`, project it with
`writer:append{start=b.start, gate=b.gate, key=b.key, velocity=b.velocity,
channel=b.channel, subchannel=b.subchannel}`. Its `count`, `index` and `selected`
fields are query metadata and must be omitted.

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

**Log.** Every plugin has a log (the last 1,000 lines, at most 1 MiB of text, UTC times): loading, what it printed,
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

Finite Lua floating-point values retain their exact IEEE754 bits through saved JSON
and reload, including subnormals and negative zero; integers retain integer storage.
This keeps author graph coordinates and view ranges stable across preset sessions.
The one MiB limit counts actual JSON encoding, including escapes and punctuation.
Repeated/nested references share that cumulative conversion allowance. A rejected
configuration update preserves the previous value even when Lua catches the error.
Oversized stored data is not loaded; oversized configuration sources are kept for
recovery. Preset listings scan at most4096directory entries and return at most1000
valid names; exceeding either limit raises an error without a partial list. Native
conversion is bounded and checks cancellation/time before and after; directory scans
also check periodically. Container/allocation overhead is additional to JSON bytes.

A run that fails drops its changes to either configuration scope. Preset save/delete
take effect immediately and remain outside musical Undo. Without a settings
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
| `text` | `max_len`, `default`, `multiline = false` | single-line text field, or a three-row editor when `multiline = true` | string |

Every parameter has a `key` (`a-z 0-9 _`, unique), an optional `label` and `help` (its
tooltip). At most 64 parameters.

`text.multiline` is an optional boolean; omitting it preserves the API 0.1 editor.
The opt-in editor accepts Enter as a newline and requires valid UTF-8 defaults.
`max_len` limits Unicode scalar values, not bytes or grapheme clusters. Text is passed
literally, including CRLF, combining marks and emoji sequences; presets store the same
string. Layout, line limits and font coverage remain the plugin author's decisions.

**`ctx`**: `params`; `track` — the track (or the clip content) the Piano Roll edits;
`notes` — a query of its selected notes (or, for `needs_selection = false` without a
selection, all its notes); `selection` (true when notes were selected); `tx` — the open
transaction labelled with the tool's name; `preview` (true while the window previews);
`seed`; `keys` (128 or 256); `tool` (its id). API0.2 adds `owner_track` — the
captured native editing Track/Subtrack handle. It equals `ctx.track` for normal
Track/Subtrack jobs; for owned/linked Clips it is the actual editing owner, while
`ctx.track` remains content. Native Piano Roll tool jobs always provide it. A direct
internal invocation without an editing track supplies nil; no placement-order guess.
Its reads use the same snapshot, grant and handle-incarnation checks as other handles.

For a custom tool opened on an owned clip, `ctx.track` is that clip's content handle
bound to the native editing track, including a Subtrack. Its note queries, packed
track_id metadata, writer/default MIDI channel and Subchannel validation use that
track even when an earlier linked placement belongs to another track. Editing still
changes the shared content visible in every linked placement. The bound handle expires
if its track is deleted/recreated or the content has no placement on that track; normal
project revision/lifecycle checks also apply. Content identity/equality is unchanged.
Ordinary content handles acquired through the API retain their existing first-placement
interpretation. This does not introduce per-note RGBA colors or new permissions.

**Preview / Apply / Cancel.** The window runs the tool on a snapshot of the project each time
its inputs change (controls, the edited notes, their selection) and draws the result notes in
the Piano Roll; the project does not change. **Apply** applies exactly the previewed
transaction through the undo history — one Undo step — or, when the project changed since the
preview, runs the tool once more and applies that. **Cancel** closes the window; nothing was
changed. Results above 2,000,000 notes are not drawn (Apply still works). Every editor rule
holds because a tool can only change the project through the API's transactions: exact ticks
and gates, 128 / 256 keys, the Conductor's global tempo, track / subtrack / clip contents,
subchannels, Selection Opacity in the preview, Undo / Redo.

Project plugin settings are outside musical history. When a command publishes
`plum.config.project` changes, the native host invalidates retained custom-tool
results and their generation, and waits for the document's native readers before
storing data. New custom runs/Apply wait until that persistence completes. A late
worker reply from the old generation is discarded. This also prevents a retained
preview from indefinitely pinning the document against a capture change. Ordinary
API0.1 musical transactions and Undo boundaries are unchanged.

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

PlumSpirit ships 22 example packages inside the program (their files are compiled in, so
no download is needed): **Plugins → Lua Plugins… → Examples**. The original API0.1
Graph Shift and CC Batch remain included. Installing copies an example into the plugins
folder like any other package (origin *Example*); review the capabilities requested by
that package and enable it through the normal plugin controls. **Reinstall**
restores the original files after you experimented with them (your decisions are kept).
The sources are also in the repository: `crates/plumspirit/examples-lua/`.

| Example | Where | What it shows |
|---|---|---|
| **Graph Shift** (`plumspirit.example.graph-shift`) | Tools → Custom → Graph Shift, and the note menu → Custom | a [custom tool](#custom-tools) with an `int`, a `curve` (the graph editor) and a `choice` parameter; streaming a query with `q:batches{columns = {"start"}}`; `ctx.tx:modify_notes` with a function (keep / change / delete); live Preview = Apply, one Undo step; deterministic (same notes and settings → the same result) |
| **CC Batch** (`plumspirit.example.cc-batch`) | Plugins → CC Batch (Example) → Write CC Over Selection… / Clear CC Over Selection… | two [commands with options dialogs](#ui-integration) (controller, Graph or LFO shape, cycles, value range, step, replace); `plum.selection.notes` / `active_track` / `time`; `t:events{…}`, `tx:delete_events`, `tx:insert_events` in one [transaction](#transactions) (one Undo step); `plum.progress`; `plum.ui.notify`; the events are saved in the `.lup` project and exported to MIDI |
| **Custom Graph Laboratory** | Plugin commands | Three authored editable graph systems, canvas interactions, presets and MIDI Preview/Undo. |
| **Custom Drawing Studio** | Tools → Custom | Five authored native-stroke tools, geometry, deterministic spray and note transactions. |
| **UI Layout Laboratory** | Plugin commands | Declarative layouts, lists, trees, controls and typed worker events. |
| **UI Image Laboratory** | Plugin commands | Native image primitives, alpha/UV crop, pointer marker and selected-image loading. |
| **Transport Monitor** | Plugin commands | Read-only bounded captured playback snapshots and observations. |
| **Transport Controls** | Plugin commands | Explicit bounded native playback control and persistent loop actions. |
| **MIDI Pattern Laboratory** | Plugin commands | Authored safe MIDI plans and isolated internal output voices. |
| **Editor Overlay Laboratory** | Plugin commands | Retained guides/Player readouts and bounded author-driven surface refresh. |
| **[Image Art Studio](../crates/plumspirit/examples-lua/image-art-studio/README.md)** | Tools → Custom, plus source commands | Bounded selected-image capture, native image-note plans/writer and mask editing; existing native Preview/Apply/Cancel/Undo, captured-source and parameter presets. |
| **[Text Art Studio](../crates/plumspirit/examples-lua/text-art-studio/README.md)** | Tools → Custom, plus ten commands | Authored multiline glyph art using original bidi/script ranges, explicit text-face + Emoji stacks, whole-grapheme coverage, shaped contours, alignment and native note transactions; Preview/Apply/Cancel/Undo and parameter presets. Color Glyph Laboratory authors an installed original-layer image, explicit fixed pixel capture/presets, and a second Subchannel MIDI tool using the same immutable capture for Preview/Apply. |
| **[Note Exchange](../crates/plumspirit/examples-lua/note-exchange/README.md)** | Plugins → Note Exchange | Registered author format, 16 KiB streamed decoding, 4,096-note writer batches, native import review/Apply/Cancel/Undo and atomic export. |
| **[MIDI Input Laboratory](../crates/plumspirit/examples-lua/midi-input-laboratory/README.md)** | Plugins → MIDI Input Laboratory, Tools → Custom | Fresh native device approval, bounded 256-message capture, exact project/preset persistence and author-written FIFO notes/CC/Bend mappings from fixed input; native Preview/Apply/Cancel/one Undo/Redo and Clip-local ownership. |
| **[Font Resource Inspector](../crates/plumspirit/examples-lua/font-resources/README.md)** | Plugins → Font Resource Inspector | Three existing resource inspections plus an authored native installed-font chooser, paged search/list, bounded scalar outline canvas, plain versioned choice/report and independent presets; no project edits. |
| **[Selected Text File](../crates/plumspirit/examples-lua/file-transform/README.md)** | Plugin command | Streams an explicitly selected UTF-8 input, uppercases ASCII in authored Lua, and atomically saves selected output. |
| **[Geometry Primitives](../crates/plumspirit/examples-lua/geometry-primitives/README.md)** | Plugin commands | Author-written curve/path/raster/Boolean/formula/random combinations using shared native geometry and note transactions. |
| **[Metadata Notebook](../crates/plumspirit/examples-lua/metadata-notebook/README.md)** | Plugin commands | Sparse namespaced Project/Track/Group attributes, explicit author schema migration and native Undo/LUP persistence. |
| **[Composable Note Service](../crates/plumspirit/examples-lua/note-service-provider/README.md)** | Pure provider | Two explicitly authored versioned services; install and enable separately from a caller. No project handles or UI access in provider requests. |
| **[Composable Services Laboratory](../crates/plumspirit/examples-lua/service-composer/README.md)** | Plugin commands | Declared provider dependencies, seeded note-pattern generation and packed selected-note transforms, staged only after successful replies. Requires the separate Note Service provider enabled with services.provide. |
| **[Reactive Project Monitor](../crates/plumspirit/examples-lua/reactive-project-monitor/README.md)** | Automatic read-only observation and summary command | Ten typed native event sources, bounded recent history, no musical edits. Requires project.read/project.observe for subscriptions. |
| **[Reactive Velocity Guard](../crates/plumspirit/examples-lua/reactive-velocity-guard/README.md)** | Explicit command and separately authorized observation | Author-written first-Track velocity rule. Automatic note responses require project.automate plus ordinary read/observe/edit grants, with native Undo and causal-loop suppression. |


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

API 0.1 and implemented API 0.2 calls, by family. Detailed signatures, scopes and limits are in the linked sections. `p` = project, `t` = track / subtrack, `c` = clip,
`q` = a note or event query, `tx` = a transaction, `s` = subchannel, `f` = folder.

| Family | Calls | Section |
|---|---|---|
| Versions | `plum.version`, `plum.api_version`, `plum.api.major / minor / patch`, `plum.require_api(major [, minor])`, `plum.lua_version` | [Versions](#versions) |
| Capabilities | `plum.capabilities()`, `plum.has(name)` | [Capabilities](#capabilities) |
| Selected files (0.2) | `plum.files.open / save`, `f:info / read / read_all / seek / write / commit / close`; byte-limit constants | [Selected files](#selected-files-api-02) |
| Images (0.2) | `plum.images.load / from_rgba`, color helpers, `mapping / compose / inverse / map_point`; image pixels/transforms/warp/fit/mask/histogram, mask info/pixel/row/read/bounds/close and limit constants | [Images](#images-and-pixels-api-02) |
| Thick strokes (0.2) | `plum.geometry.rasterize_stroke`, `stroke_miter_limit` | [Stroke rasterization](#thick-stroke-note-cell-rasterization-api-02) |
| Raster Boolean (0.2) | `plum.geometry.raster_boolean`, `max_raster_boolean_input_spans` | [Boolean rasters](#boolean-raster-composition-api-02) |
| Image-note plans (0.2) | `image:note_plan`, plan `info / palette / next / reset / close`, writer `append_plan`, bounded plan constants | [Image-note plans](#bounded-image-note-plans-api-02) |
| Note-mask samplers (0.2) | `mask:note_sampler`, sampler `sample / sample_batch / count_notes / info / close` | [Note-mask sampling](#image-mask-note-sampling-api-02) |
| Mask-conditioned writes (0.2) | `tx:mask_notes(query, sampler, {action,changes?,mode?})` | [Mask transactions](#native-mask-conditioned-note-transactions-api-02) |
| Unicode segmentation (0.2) | `plum.text.grapheme_count / graphemes`, `unicode_version` and segmentation limits | [Unicode text](#unicode-text-api-02) |
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
| Custom tool `ctx` | `params`, `track`, `notes`, `selection`, `tx`, `preview`, `seed`, `keys`, `tool`; API 0.2 adds `owner_track` and optional `stroke` | [Custom tools](#custom-tools) |
| Package assets (0.2) | `plum.plugin.asset(relative_path)` for bounded private package streams | [Private assets](#private-package-assets-api-02) |
| Transport (0.2) | snapshots/observations, validated transport requests and staged loop settings | [Snapshots](#transport-snapshots-and-observations-api-02), [requests](#transport-requests-api-02), [loops](#staged-loop-settings-api-02) |
| MIDI plans / input (0.2) | bounded authored event plans, isolated native output, separately approved device input | [Plans](#bounded-midi-event-plans-api-02), [output](#isolated-native-midi-plan-output-api-02), [input](#live-midi-input-api-02) |
| Reactive / services (0.2) | `plum.plugin.observe / unobserve`, bounded transaction responses and declared worker services | [Observation](#reactive-observation-transport-api-02), [responses](#bounded-transaction-responses-api-02), [services](#service-providers-api-02) |
| Sparse attributes / formats (0.2) | namespaced typed metadata/history and author-written import/export adapters | [Metadata](#typed-project-metadata-api-02), [formats](#registered-format-adapters-api-02) |
| Native geometry / formulas (0.2) | pure affine, sampled curves/splines, bounded formula/random primitives, clipping and predicates | [Geometry](#shared-geometry-primitives-api-02), [adaptive curves](#adaptive-cubic-flattening-and-geometry-predicates-api-02) |
| Paths / regions (0.2) | adaptive mixed paths, polygon/path note-cell rasters and multicontour regions | [Paths](#adaptive-mixed-path-flattening-api-02), [cell rasters](#polygon-and-path-note-cell-rasterization-api-02), [regions](#multi-contour-cell-regions-api-02) |
| Image composition / masks (0.2) | native image composition, mask colorization and span masks | [Composition](#native-image-composition-api-02), [colorization](#native-mask-colorization-api-02), [span masks](#geometry-span-masks-api-02) |
| Font resources / shaping (0.2) | builtin/selected/installed fonts, original outlines, coverage runs, shaped runs and supported color layers | [Resources](#bounded-font-resources-and-scalar-metrics-api-02), [catalogs](#installed-font-catalogs-api-02), [outlines](#original-cubic-glyph-outlines-api-02), [coverage](#font-coverage-runs-api-02), [shaping](#shaped-glyph-runs-api-02), [color](#color-glyph-layers-api-02) |
| Custom UI / graphs (0.2) | declarative panels/canvas/events, bounded images and retained resources; author-controlled graph mathematics | [Panels](#declarative-plugin-panels-and-canvas-api-02), [images](#bounded-canvas-image-resources-api02), [retained images](#project-scoped-retained-ui-images-api-02), [graph example](#author-written-custom-graph-example) |
| Editor surfaces / drawing (0.2) | captured view snapshots, staged overlays, surface events and authorized completed Piano Roll strokes | [Views](#immutable-native-view-snapshots-api-02), [overlays](#staged-editor-overlay-frames-api-02), [surface events](#surface-notifications-and-exact-reactive-view-delivery-api-02), [strokes](#custom-piano-roll-strokes-api-02) |

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

<a id="reactive-observation-transport-api-02-development"></a>

## Reactive observation transport (API 0.2)

`plum.plugin.observe{ id, events={...}, debounce_ms=100, run=function(event) ... end }`
registers an observer while loading/registering. It requires both `project.read` and
`project.observe` for project events; transport-only subscriptions instead require
`playback.read` as described above. surface_changed requires ui.overlay plus project.read
or playback.read, and delivers only the surface authorized by that read permission.
It does not require project.observe. `plum.plugin.unobserve(id)` removes a subscription,
including from a callback.
IDs use the same 1–64 ASCII identifier rules as commands. At most 32 subscriptions;
plain options and dense event-name arrays; debounce is 20–5000 ms.

Current envelope: `{schema=1, context="metadata", subscription, kind, project,
revision, sequence, first_sequence, coalesced, dropped, transaction_allowed, read_only=true, data={...}}`.
Integers remain exact. `dropped` is cumulative queue overflow/stale/resource loss for
the current native runner. The twelve native-delivered kinds and `data` fields are:

| Kind | Metadata |
| --- | --- |
| `project_opened` | `tracks`, `notes` (stored Track and owned Clip notes), `keys` |
| `project_closed` | empty table; envelope identifies the closed project |
| `content_changed` | `previous_revision` (earliest coalesced history state) |
| `selection_changed` | selected stored `notes`, selected `tracks`, selected `clips` |
| `tracks_changed` | `structure_revision`, `tracks` |
| `clips_changed` | Clip-table `layout_revision`, explicit `placements` count |
| `undo`, `redo` | `previous_revision`; successful native history actions only |
| `saved` | `format="lup"` or `"midi"`; successful native completion only |
| `view_changed` | `active_track` (stable native ID), `keys`, `viewport` |
| `transport_changed` | captured native transport snapshot; see playback.snapshot above |
| `surface_changed` | `surface="piano_roll"/"player"`, `visible`, optional exact `view_revision` |

`viewport` contains `origin_tick` as exact unsigned decimal **text**, plus finite
`pixels_per_tick`, `scroll_rows`, `row_height` and `top`. No file paths or pointer
positions are exposed. `revision` identifies the current native history state at
notification time; a save signal is not a retained snapshot of the saved file.
Selection counts cover Track and owned Clip stores once, not linked appearances.
Selection signals also invalidate when selected note ranks/values change; counts
may be unchanged. Track signals cover structure/properties/Subchannels/mute/solo and
folder layout; Clip signals cover placement and shared source-content changes.
Legacy view_changed signals cover the Piano Roll viewport, active Track/Clip and key
range. surface_changed separately covers authorized rendered Roll/Player geometry,
visibility, resize/DPI and exact view generation; see the overlay section below.

Native metadata collection starts at most every 50 ms, with a fixed-size cursor,
at most 2,048 metadata/selection-ID visits per step and a cooperative 2 ms step
budget checked every 64 visits. It never traverses note payloads, copies selections
or retains the live document. Large metadata layouts finish over multiple frames;
ongoing revision/structure changes restart collection. Signals are eventually
coalesced invalidations, not atomic selection snapshots or a complete audit history.
No sampling runs without active subscriptions. Source registration invents no musical
algorithms. There is no replay of events before subscription readiness.

The installable [Reactive Project Monitor](../crates/plumspirit/examples-lua/reactive-project-monitor/README.md)
shows all kinds, bounded author-owned history and a summary command.

Observation callbacks are metadata-only: they do not receive a project World
(`plum.project.current()` is nil). An optional transaction response runs separately
with a detached read snapshot, as described below. Callbacks run on the existing Lua worker with a 250 ms
budget, never on UI/audio threads. Rust authority rejects project transactions, edits,
UI selection and project-config writes even through nested registered commands. This
scope cannot be changed by editing the event table or the Lua lifecycle phase.
Observation status messages are revision/lifecycle-checked again after earlier results
in the same application poll have been applied. Use
explicit user commands/tools for existing edits. Notifications and plugin-owned local
settings retain their existing bounds. File prompts are not user-authorized in observers.

Queues contain at most 64 metadata records per plugin, with native quota reservation;
no document roots or note tables are queued. Repeated subscription/kind/project events
coalesce to latest values, preserving first/latest sequence and the earliest previous
revision for content/history ranges. Deferral is capped at max(500 ms, debounce_ms).
Oldest queued records are dropped at capacity. Dispatch is limited to eight jobs/sec/
plugin; user commands/tools preempt observation. This is a lossy latest-state signal,
not an audit log. Stale revisions/projects are discarded. Reload, disable, permission
withdrawal, project close and replacement invalidate queued/running deliveries; stale
results/notices are discarded. Any automatic handler error pauses observation until
reload, while ordinary commands remain usable. Legacy API 0.1 job behavior remains.

<a id="bounded-transaction-responses-api-02-development"></a>

## Bounded transaction responses (API 0.2)

Inside a valid observation callback, `plum.plugin.request_transaction{command="id",
data=value}` offers one response using a command registered by the **same plugin**.
It requires `project.read`, `project.observe`, `project.automate` and `notes.edit`.
It returns `false` when the native event's causal policy disallows a transaction;
`true` means the request was offered, not that Apply is guaranteed. User work, shared
cooldown, quotas, stale input, permission withdrawal or lifecycle changes may discard it.
Requests from ordinary commands, nested responses or forged event tables are refused.

The separate worker invokes the author command with its ordinary `ctx.command` and
parameter defaults plus `ctx.reactive=true`, `ctx.event` (the original typed cause) and
`ctx.request` (the transferred data). No author source strings are generated/evaluated
from data. The World captures the project at response start; the cause is a coalesced
invalidation rather than an atomic snapshot. The worker detaches the root before Lua
calculation. Apply validates the history/project, exact native store/property/Clip/
Conductor counters and captured UI selection, then reuses native history operations.
Notifications from the response are held until this validation and successful Apply.

One response may commit one musical transaction with at most eight batched native
operations and 100,000 created/changed/removed notes. Mutation queries stop at 100,000
matches. Current automatic scope supports note edits and existing packed note metadata
(including assignment to unlocked existing groups). Structural, Conductor, controller
event, group-table, explicit selection and project-config changes require an explicit
command. This keeps automatic scope smaller than user-invoked commands. User commands
retain all API 0.1 capabilities and budgets. Native group/ancestor locks still apply.

Snapshot admission allows at most 4,096 current native identity objects in total:
Tracks, Subchannels, explicit Clips, owned contents, groups and folders. Selected Track/
Clip lists are also bounded to 4,096. A cooperative 250 ms budget covers preflight,
copy and author execution; opaque native clone/history phases check before/after.
Shared native quota reservations conservatively cover copied metadata, possible history/
COW overlap and 64 MiB staging. Large projects can be refused below other caps; use an
explicit command for those. This is not a claim of 50M-note automatic editing acceptance.

Automatic note iterators charge native quota, allow at most 32 creations per job and
release native snapshots eagerly at job end, even when Lua retains their closures.
Those retained closures return nil. API 0.1 ordinary iterators keep their behavior.

All plugins share 500 ms response admission spacing. An automatic child revision cannot
start another automatic edit, including in another plugin. Undo/Redo are blocked as
causes and preserve the restored result. A later external musical revision can start
another response. After child content settles, an external selection-only change also
starts a new cause without a musical revision. Continuous/coalesced changes are lossy;
this is not an edit audit or a promise to react to every input gesture.

`data` accepts nil, booleans, exact signed integers, finite numbers, UTF-8 strings,
dense arrays and string-key objects. Limits: 64 KiB conservative encoded size, 2,048
values, nesting depth 8, 16 KiB/string and 128 bytes/key. Empty tables are objects.
Metatables, cycles, sparse/mixed-key tables, functions, userdata and nonfinite numbers
are refused before transfer. Native buffers are quota-reserved and shared without
repeated deep copies; Lua decoding uses the receiving VM's memory quota.

See the installable [Reactive Velocity Guard](../crates/plumspirit/examples-lua/reactive-velocity-guard/README.md)
for a complete author-written observer, request and command. Native headless App tests
verify actual Apply/Undo/Redo, competing plugins, later selections, cancellation,
stale selection/notices, locks and disable cleanup. Interactive supported-target Windows
and broader performance acceptance remain open release gates.

<a id="service-providers-api-02-development-foundation"></a>

## Service providers (API 0.2)

Services exchange bounded plain data on plugin workers. Their payloads exclude
private Lua objects and project handles.

A plugin granted `services.provide` may register while loading:

```lua
plum.plugin.service{
  id="double", version="1.2",
  run=function(data, ctx)
    -- Entire algorithm belongs to the plugin author.
    return {value=data.value * 2, source_tick=data.source_tick}
  end
}
```

The native declaration is namespaced `<plugin-id>/double`. At most 32 services per
plugin; local IDs follow the 1–64 ASCII command-ID rules. Versions are `major.minor`,
each component 0–65535; a provider accepts a requested identical major with a minor
no newer than its own. Duplicate IDs, invalid versions, metatables and late or automatic
registration are refused. Disabling/reloading removes declarations with the runner.

Native dispatch executes on the existing provider worker with a cooperative 250 ms
budget and one in-flight job per worker. Input and reply use the bounded structured
format documented above. `ctx` contains the namespace, actual provider version and
an exact supplied integer `seed`; author algorithms decide how to use it. No Lua
references or userdata cross the boundary. Replies from expired/canceled/failed jobs
are discarded. Native lifecycle checks and registration-phase restoration are enforced.

Provider jobs receive no project World and cannot start transactions, change selection,
post UI notices, access settings/presets, prompt for files or register/unsubscribe host
interfaces. Nested commands and spoofed registration phases do not bypass that scope.
The provider's Lua functions and globals remain its own; clients receive only
validated plain replies. This does not certify arbitrary author algorithms as pure or
deterministic: versioned, explicit inputs/seeds and deterministic author code remain
necessary. Actual installed-package tests exercise worker calls and native transaction composition.
A separate isolated Windows application smoke verifies a real client/provider reply.
Mouse-driven workflows and final supported-target acceptance remain open.


### Calling a declared service

The caller requests `services.call` and declares up to 32 dependencies in its
`plum-plugin.toml` (API 0.2 or later):

```toml
service_dependencies = ["example.provider/double@1.0"]
```

The provider must already be installed, enabled, ready and granted `services.provide`.
Dependencies never install plugins or grant permissions automatically. Matching uses
the same major version and a provider minor at least the declared minimum. A call's
optional version may raise that minimum within the declared major:

```lua
local reply = plum.services.call("example.provider/double",
  {value=21, source_tick=9007199254740993},
  {version="1.0", seed=17, timeout_ms=250})
assert(reply.value == 42)
```

Options default to the declared version, integer seed 0 and 250 ms. `timeout_ms` is
an integer 10–250; seed is an exact signed integer. Inputs and replies use the same
64 KiB/2,048-value/depth-8 bounded structured format as transaction requests. Invalid
values, absent dependencies, incompatible versions, denied permissions and expired
workers produce errors catchable with `pcall`.

Provider effective capabilities are the intersection of the caller's inherited
capabilities and its own grants; publishing permission is checked separately. `plum.has`
and `plum.capabilities()` reflect that intersection during dispatch, then restore the
provider's normal grants. A provider receives no project World even if both parties
have project permissions. Service scope denies project writes, settings/presets,
UI/file prompts and new registrations. Nested calls also require the provider's own
declared dependency and effective `services.call` permission.

Calls wait on the caller worker, never the UI/audio thread. Limits: one outgoing
request slot per worker, 64 in-flight broker edges, eight queued/running calls per
provider, four actively computing service workers, eight plugin workers per chain
including its root, and 64 calls per root job including descendants. Workers waiting
on children do not consume a compute slot. Owner reentry, inherited cycles and global
cross-root wait cycles are rejected. Timeouts share the earliest ancestor deadline;
waiting does not extend the root job budget. Cancellation, reload/disable, grant
withdrawal and project revision/close invalidate affected queued/results. Standalone
plain-data jobs have no project binding. Replies contain no host capabilities.

Registered format adapters can also call declared pure services while retaining
their narrowed format authority. See [codec composition](#composing-codecs-with-declared-pure-services)
for protected import/export staging and the unchanged payload/lifecycle limits.

Call providers before opening a transaction where practical. Author code validates
returned data and stages supported edits using the existing transaction API. Uncaught
worker failure discards even privately committed steps; successful Apply is one normal
native transaction with Undo/Redo. External file publication retains its separate
explicit boundary. Seeds do not make arbitrary provider code deterministic: authors
must use explicit inputs and deterministic algorithms for Preview/Apply parity.

Install [Note Service Provider](../crates/plumspirit/examples-lua/note-service-provider/README.md)
and [Service Composer](../crates/plumspirit/examples-lua/service-composer/README.md) together.
The provider explicitly implements pattern/transpose algorithms in Lua. The client
creates 16 notes or transforms at most 2,048 selected notes in bounded 32-note batches,
including optional packed metadata. Missing providers and failures leave the project
unchanged; native group locks, key ranges, Clip ownership and MIDI colors still apply.


<a id="typed-project-metadata-api-02-development"></a>

## Typed project metadata (API 0.2)

`plum.metadata.project()` returns `value, version` from the installed plugin's own
namespace in the current project. Missing data returns `nil, nil`; a stored nil has
a nonnil version. Requires `project.read` and a current World. This is separate from
API 0.1 `plum.config.project`, whose persistence and non-Undo behavior are unchanged.

```lua
local old, version = plum.metadata.project()
local tx = plum.transaction{label="Author attributes"}
tx:set_project_metadata({title="Study", tick=9007199254740993},
  {version=2, expect_version=version or 0})
tx:commit()
```

`tx:set_project_metadata(value, {version=N, expect_version=V})` requires project.read
and metadata.edit. `version` is a positive exact32-bit integer; optional expect_version
compares against the stored version (0 means absent). Mismatch refuses the write.
`tx:remove_project_metadata({expect_version=V})` removes only the calling plugin's
record. Omit the optional comparison to replace/remove its current record. Native
namespace selection cannot be supplied or spoofed through a Lua argument.

Use the existing bounded plain-data types: nil, boolean, signed64-bit integers,
finite numbers, UTF-8 text, dense arrays and string-key tables; no metatables,
cycles, executable values, sparse/mixed tables, NaN/infinity or opaque handles.
Per value:64KiB structured size,2,048 nodes,depth8,16KiB text,128-byte keys.
Native persistence additionally limits each record64KiB,128 namespaces and the
complete encoded table1MiB. At most32 write attempts per worker job, with native temporary/
retained-memory quota accounting before staging. Reading produces a Lua-owned copy;
mutating it changes nothing until explicitly staged.

Writes/removals use native staged transactions: reads see staged data, Cancel or
failed execution leaves nothing, stale Apply refuses, and successful Apply/Undo/Redo
uses normal history. Project metadata is excluded from automatic note responses and
pure service jobs. It is saved in the optional native XMDA section with a bounded,
typed binary layout; unknown plugin namespaces/schema versions survive reopening.
Persistent Undo journal patches include metadata and journal identity checks it.
MIDI export excludes these attributes and remains byte-identical for unchanged music.
Unsupported future native layouts fail clearly instead of silently dropping data.

Authors implement migrations explicitly: read the old version, validate/convert it,
then write with a newer version plus expect_version inside a transaction. The host
never guesses a schema or runs a migration on the UI thread. Plugin removal leaves
its project data intact for reinstall; the same owner can remove it transactionally.
Project duplication/Save As retains project attributes, as it retains other project
state. Track/Subtrack associations are also implemented below. Clip/Subchannel and per-note
attributes remain unexposed. No IDs were added to packed notes.

See [Metadata Notebook](../crates/plumspirit/examples-lua/metadata-notebook/README.md).
Tests cover actual headless App command Apply, Undo/Redo, background LUP save/reopen,
legacy PLUG coexistence, typed limits and persistent-journal restart. Interactive
Windows metadata UI and final release acceptance remain open.


### Track and Subtrack attributes

`plum.metadata.get(handle)` returns own-namespace `value, version` for a current
Project, Track, Subtrack or permanent Group handle. `tx:set_metadata(handle, value, opts)` and
`tx:remove_metadata(handle, opts?)` use the same permissions, types and version
comparison as the project shortcuts. Unsupported handle families fail clearly.

```lua
local track = plum.project.current():track(1)
local data, version = plum.metadata.get(track)
local tx = plum.transaction{label="Track attributes"}
tx:set_metadata(track, {role="melody", strength=0.75},
  {version=1, expect_version=version or 0})
tx:commit()
```

Associations use native stable Track IDs, including Subtracks, with no per-note ID.
Renaming/moving a Track preserves its attributes. Native subtree deletion removes all
owners' attributes for the removed Tracks inside the same history boundary; Undo
restores both objects and attributes, Redo removes them again. Independent copied/new
Tracks receive new IDs and start empty. Save As retains the existing project and Track
attributes. Clips do not receive metadata or copy it: a Track association follows its
Track ID, regardless of clip placement/link changes. Object duplication can explicitly
copy the calling author's validated data with normal transactions when desired.

Native target records are sparse:4,096 total records,128 distinct owner namespaces,
1MiB complete encoding,64KiB each. Every referenced Track must exist on Apply/save/load;
orphan native records are refused. The tagged target encoding reads the earlier
project-only representation, whose bytes remain unchanged for project-only stores.
Legacy PLUG settings and MIDI bytes retain their behavior.

New Lua handles created for private Tracks share a quota-accounted native lifetime
token. Cancel, implicit cancellation, failed jobs and discarded/refused results expire
them; they cannot address later objects reusing their IDs. Before Apply they are valid
only in their originating job; successful Apply keeps them valid under the existing
native incarnation checks. Reacquiring a handle after a private commit retains that
guard. Tokens allocate lazily only when a born handle is exposed; note-only transactions
pay no token charge. A shared128-byte native reservation stays while Lua retains a
token, including unreachable userdata until garbage collection. Handle keys name native
IDs/incarnations; always check freshness rather than persisting private keys across a
canceled transaction. Reacquire handles after a refused Apply/without-Undo fallback.


### Permanent Group attributes

The same `plum.metadata.get(group)`, `tx:set_metadata(group,value,opts)` and
`tx:remove_metadata(group,opts?)` now support existing permanent native Groups.
Read requires project.read; writes also require metadata.edit and an unlocked Group
and all its ancestors. Setting attributes never grants the ability to unlock a Group
or edit notes. Existing native group changes still need notes.edit. Temporary Groups
are refused because their identity is omitted from saved projects.

Native SetGroups removes every owner's attributes when a Group is deleted or made
temporary, in the same Undo boundary. Undo restores Group state and attributes; Redo
repeats cleanup. Group rename/reparent/lock/link changes retain attributes as long as
the target remains permanent. Linked Group identities keep independent metadata;
new/copied Groups start empty. Authors may explicitly copy their own data using the
normal transaction APIs. Removed plugins' namespaces survive on live eligible targets.

The native tagged codec includes Group IDs and validates target existence/permanence
on Apply/save/load. Shared limits remain4,096 total records/128 owners/1MiB table;
there is no new per-note column. Metadata Notebook demonstrates annotation/removal
of the first existing permanent Group without creating an automatic artistic function.
Its headless App test verifies Project/Track/Group commands, Undo/Redo and background
LUP save/reopen. Separate native disk tests verify Group deletion/temporary transitions,
foreign-owner cleanup and persistent-journal restart. An isolated actual Windows read
probe verifies exact Project/Track/Group values, ancestor-lock refusal and canceled
staging; it does not certify mouse-driven editing or final release acceptance.


<a id="shared-geometry-primitives-api-02-development"></a>

## Shared geometry primitives (API 0.2)

`plum.geometry` provides pure mathematical services on the plugin worker. It needs no
project or extra permission and reuses the native Draw/generator geometry and existing
image affine implementation. It creates no notes, graph window, pointer hook or GPU
access. Authors define the artistic algorithm and use separately permitted transactions
when they want musical edits. Native API 0.1 curve and Draw/Paint interfaces coexist.

| Function | Implemented behavior |
| --- | --- |
| `mapping(options)` | Same descriptor and pivot/scale/degree rotation/translation order as `plum.images.mapping`. |
| `compose(first,second)` | Apply first then second, returning a plain affine descriptor. |
| `inverse(matrix)` | Validated inverse using the existing conditioned native affine helper. |
| `map_point(matrix,x,y)` | Return two mapped local coordinates. |
| `map_points(matrix,columns)` | Return a Lua-owned bounded `{count,x,y}` batch; source columns unchanged. |
| `contains(box,x,y)` | Inclusive native rectangle containment. |
| `clip_segment(box,x0,y0,x1,y1)` | Return nil outside, or `{x0,y0,x1,y1,t0,t1}` for the native clipped parameter interval. |
| `polyline_length(columns)` | Native sum of Euclidean segment lengths; a single point has length0. |
| `resample_polyline(columns,segments)` | Native arc-length sampling, including endpoints; returns `{count,x,y}`. |
| `max_points` | Constant4096. |

A column descriptor is `{x={...}, y={...}, count=optional_integer}`. Arrays must be plain,
dense, equal-length and contain1–4096 finite numeric points. Optional count must match.
Outputs can be fed directly into the other column operations. Source tables remain
unchanged. Extra fields, metatables, holes, mixed keys and nonnumeric/nonfinite points
are refused. Box schema is `{x_min,y_min,x_max,y_max}`, ordered minima/maxima; zero-width
boxes are allowed. Coordinates and matrix coefficients use the shared finite +/-1e12
limits; inverse conditioning and mapped-output limits also apply. Aggregate polyline
length can exceed1e12. These are local continuous doubles, not exact absolute MIDI ticks.
Keep large song origins in integers and add offsets with `plum.ticks` checked arithmetic.

```lua
local g = plum.geometry
local path = {x={0,3,3}, y={0,0,4}}
assert(g.polyline_length(path) == 7)
local sampled = g.resample_polyline(path, 7) -- 8 points
local map = g.mapping{scale_x=2, scale_y=-1, translate_y=4}
local transformed = g.map_points(map, sampled)
local box = {x_min=0,y_min=0,x_max=6,y_max=4}
local visible = g.clip_segment(box, -10,2,10,2)
assert(visible.x0 == 0 and visible.x1 == 6)
```

`segments` is a Lua integer1–4095. A path with two or more points returns segments+1
samples, even when all points coincide. A one-point path returns that one point.
The existing native clipper accepts a positive parameter interval and excludes contact
at only one corner; an inside zero-length segment retains its [0,1] parameter interval.
Floating-point roundoff remains possible; compare calculated coordinates with a suitable
local tolerance. Algorithms and bounds are deterministic for identical supported inputs
on the same host; author code remains responsible for Preview/Apply inputs/seeds.

Input/output native buffers reserve shared native quota before allocation, then release
when the call returns or fails. Column input, mapping and output loops check cancellation
at least every64 points. Native length/resampling is a bounded linear phase between
checks, at most4096 input/output points. Lua output tables use the normal VM quota.
There is no persistent native buffer/cache or per-note allocation. Large graph datasets
must be sampled/aggregated before producing interactive elements; the complete graph,
shape rasterization, spline and canvas/pointer APIs remain separate unfinished work.

Install [Geometry Primitives](../crates/plumspirit/examples-lua/geometry-primitives/README.md).
Its Lua author explicitly defines a triangle, orientation and mapping to33 MIDI notes;
the host supplies only the math services. The executable test verifies one native
transaction and Undo/Redo. Five source/integration tests cover shared affine parity,
clipping, endpoint/degenerate/max-size sampling, invalid inputs, native quota and cancel.
A separate actual Windows worker smoke maps4096 points and resamples257 points64 times;
it is a bounded tiny-project smoke, not full graph/drawing or dense-MIDI acceptance.


<a id="seeded-randomness-and-authored-parametric-formulas-api-02-development"></a>

### Seeded randomness and authored parametric formulas (API 0.2)

`geometry.random_values(seed,indices[,options])` returns a plain dense numeric array
with one bipolar value per supplied index. The1–4096indices are exact nonnegative Lua
integers (0..2^63-1); duplicates repeat and output keeps input order. Seed and optional
`salt` (default0) are exact Lua integers, including negative values interpreted as
64bit words; floats/strings are refused. Each sample uses the existing native
`Rng::for_index`, so order, paging and other random calls do not advance shared state.
The same seed/salt/index/distribution gives the same value in the same host.

Options are a plain table with only `salt` and `distribution`. Distribution defaults
to `"uniform"` and is one of these bounded native primitives:

| Distribution | Value |
|---|---|
| `uniform` | Uniform [-1,1). |
| `gaussian` | Standard normal divided by3, clamped to[-1,1]. |
| `triangular` | Sum of two uniform[0,1) samples minus1. |
| `exponential` | Random sign times an exponential magnitude divided by3, clamped to[-1,1]. |

This is deterministic artistic randomness, not cryptography. The host does not choose
how randomness maps to notes, colors, geometry or brush behavior.

`geometry.formula_points(parameters,{x="...",y="..."})` returns packed `{count,x,y}`
for1–4096plain dense finite parameters within +/-1e12, retaining order/duplicates. The
plugin authors both formulas. This reuses the existing native math-only Curve expression
parser, with variable `t` (the supplied parameter, not automatically normalized),
constants `pi`/`e`, arithmetic `+ - * / ^ % //`, parentheses, and these functions:

`abs min max sqrt sin cos tan exp ln log pow clamp floor ceil round trunc sign frac
mod fmod asin acos atan atan2 hypot`

Sine/cosine take radians. `%` and `//` use floored modulo/division; `ln` is natural
log and `log` is base10. `math.` aliases use the native Lua-compatible forms, including
`math.log(x)` natural log and `math.log(x,b)` base-b. Power is right-associative and
binds more tightly than leading minus. There are no assignments, loops, strings,
recursion, arbitrary function calls, OS/file/network access or `math.random` in formulas.
Lua authors can also use the sandbox's ordinary `math` library in their own code.

Each expression allows at most1000characters/4000UTF-8bytes,512AST nodes and64nesting
levels, as enforced by the existing parser. Combined node-evaluations per call are
bounded by `geometry.max_formula_work` (4194304). Native AST/token/output workspace is
preflighted before parsing, loops check cancellation every64samples, and VM allocation
refusal leaves no partially returned table or project edit. Parser/domain errors name
the axis and evaluation sample; final NaN/infinity or coordinates outside +/-1e12 fail
the call. No compiled expression or native cache persists after the call.

The following author-written tool core chooses its own formula, indexed velocity
variation, exact rational time mapping and musical transaction:

```lua
plum.require_api(0,2)
local project=assert(plum.project.current())
local g=plum.geometry
local parameters,indices={},{}
for i=0,31 do parameters[i+1]=i/31;indices[i+1]=i end
local path=g.formula_points(parameters,{x="t",y="5*sin(4*pi*t)"})
local random=g.random_values(7182,indices,{salt=3,distribution="triangular"})
local first_key=project:key_range()==256 and 200 or 60
local tx=plum.transaction{label="Authored parametric melody"}
local writer=tx:note_writer(project:track(1))
local notes={start={},gate={},key={},velocity={}}
for i=1,path.count do
  notes.start[i]=plum.ticks.add(120,plum.ticks.muldiv(indices[i],960,31,"round"))
  notes.gate[i]=24
  notes.key[i]=first_key+math.floor(path.y[i]+0.5)
  notes.velocity[i]=80+math.floor(20*random[i]+0.5)
end
writer:append(notes)
writer:finish()
tx:commit()
```

Use `plum.ticks.muldiv` for exact rational tick conversion and `plum.ticks.add/sub/mul`
for checked tick arithmetic. Geometry coordinates and formulas are floating point;
keep large absolute song ticks as exact integers and use local offsets for geometry.
Image color/palette/alpha/mask helpers remain in `plum.images`; these primitives compose
with them, custom graphs, brushes, sampled splines and existing note writers.

<a id="native-spline-evaluation-api-02-development"></a>

### Native spline evaluation (API 0.2)

These pure worker-side primitives reuse the native Arc generator's spline mathematics;
they do not call the native Curve editor or invent a graph or note-art algorithm.
Controls use the usual plain dense `{x={...},y={...},count?}` columns,1–4096points,
finite coordinates within +/-1e12. `kind` is required and is exactly `"catmull_rom"`
or `"b_spline"` (bounded text, at most32bytes).

| Call | Result |
|---|---|
| `geometry.spline_point(points,t,kind)` | Two coordinates at finite numeric t in[0,1]. |
| `geometry.spline_points(points,parameters,kind)` | `{count,x,y}` for1–4096plain dense numeric parameters, in supplied order including duplicates. |
| `geometry.sample_spline(points,{kind=...,steps=32})` | Uniform parameter samples at i/steps, including parameters0and1; steps is an integer1–4095. |

Catmull–Rom is uniform, passes through control points at t=i/(n-1), and reflects
end neighbors. It can overshoot the control hull. The cubic B-spline uses repeated
end controls and n+1uniform parameter spans for n>=2; it approaches the internal
controls and reaches the first/last mathematically. One control is a constant curve.
Both retain native floating-point rounding; sampling is by parameter, not arc length.
No silent coordinate clamp changes spline mathematics: nonfinite output or overshoot
outside +/-1e12 fails the entire call. Use local offsets and smaller coordinates.

Input/output native workspace is reserved before allocation, allocation errors return
without changing controls, checks run every64samples, and packed Lua output obeys the
VM quota. No persistent spline resource, native callback, project access permission or
unbounded native sampling job is created. Same inputs/kind produce the same result in
the same host. Existing cubic/path/legacy curve behavior is unchanged.

This author-written example interprets the sampled Y axis as pitch and X as local time.
The host supplies spline evaluation and the existing transaction, while the plugin
chooses its control points, sample count and musical mapping:

```lua
plum.require_api(0,2)
local project=assert(plum.project.current())
local g=plum.geometry
local sampled=g.sample_spline({x={0,4,8,12},y={0,9,-3,6}},
  {kind="catmull_rom",steps=24})
local first_key=project:key_range()==256 and 200 or 60
local tx=plum.transaction{label="Authored spline melody"}
local writer=tx:note_writer(project:track(1))
local batch={start={},gate={},key={},velocity={}}
for i=1,sampled.count do
  batch.start[i]=120+math.floor(sampled.x[i]*120+0.5)
  batch.gate[i]=30
  batch.key[i]=first_key+math.floor(sampled.y[i]+0.5)
  batch.velocity[i]=100
end
writer:append(batch)
writer:finish()
tx:commit()
```

Compose the packed output with `map_points`, `resample_polyline`, hit-testing,
`rasterize_polyline` or `rasterize_stroke`. Fixed sampling has no tolerance guarantee;
choose enough samples for the authored use. Automatic closure, centripetal splines,
custom knot vectors and adaptive spline flattening are not part of these three calls.

<a id="cubic-curves-and-mixed-paths-api-02-development"></a>

### Cubic curves and mixed paths (API 0.2)

The following pure worker functions share the existing native Custom Shape cubic
evaluator. Control coordinates stay in the author's local domain, including negative
values and non-monotonic X. They are not converted into the native normalized curve
parameter or editor. Every control coordinate must be finite within +/-1e12.

| Function | Result |
| --- | --- |
| `geometry.cubic_point(control,t)` | Two numeric coordinates at finite parameter t in[0,1]. |
| `geometry.cubic_points(control,parameters)` | Bounded `{count,x,y}` batch in the exact supplied parameter order, including duplicates. |
| `geometry.sample_cubic(control,segments)` | Uniform parameter samples at i/segments, including both exact endpoints. |
| `geometry.sample_path(path,options?)` | Ordered line/cubic samples with optional explicit straight-line closure. |
| `geometry.max_path_segments` | Constant256. |

`control` uses the existing plain column format, with exactly four points:
`{x={p0x,p1x,p2x,p3x}, y={p0y,p1y,p2y,p3y}, count=optional_4}`. The two interior
points are the Bézier handles. `t` is the curve's intrinsic parameter, independently
of its X/Y coordinate domains. Endpoints are returned directly from the controls.
Interior evaluation is kept within the control-point bounding box to contain
floating-point drift at its edges. This preserves composability with bounded geometry
columns; it is not a normalization of graph axes or author-defined musical values.

`parameters` is a plain dense numeric array of1–4096 finite values in[0,1]. It need
not be sorted. `segments` is a Lua integer1–4095 and produces segments+1 points.
Results can be passed directly to map_points, polyline_length and resample_polyline.
These are fixed sampling steps, without an adaptive geometric error guarantee.

```lua
local g=plum.geometry
local control={x={-10,-5,15,20}, y={2,30,-20,7}}
local x,y=g.cubic_point(control,0.5)
local chosen=g.cubic_points(control,{0,0.25,0.9,1})
local sampled=g.sample_cubic(control,64)
local evenly_spaced=g.resample_polyline(sampled,32)
```

A path has one authored start and a dense array of0–256 segments:

```lua
local path={start={x=-10,y=20},closed=true,segments={
  {kind="line",x=0,y=20},
  {kind="cubic",x1=0,y1=30,x2=10,y2=30,x=10,y=20},
  {kind="line",x=10,y=10},
}}
local sampled=plum.geometry.sample_path(path,{curve_steps=16})
```

The current point starts at `start`. A line appends its endpoint once. A cubic starts
at the current point, uses its explicitly authored x1/y1 and x2/y2 handles, and adds
curve_steps samples excluding its already-present start. It then becomes the current
point. With `closed=true`, the sampler appends the original start if the last endpoint
is different. It never invents a curved closing segment. Empty segment arrays return
one point. Degenerate/repeated authored vertices remain; no implicit simplification.
`closed` defaults false and must be a boolean. `curve_steps` defaults32, must be a Lua
integer1–4095 and applies independently to each cubic; lines add only their endpoint.

The complete result must fit4096 points, including the initial point and any closing
point. The host validates every segment and preflights the complete point count before
allocating the output buffer. Larger paths must be split or sampled with fewer steps.
Only the shown fields are supported; unknown kinds/fields, metatables, holes, cycles,
nonfinite/out-of-range numbers and invalid parameter domains fail clearly. In
particular, a tolerance option is not supported by this fixed sampler. Multiple
contours/holes, spline fitting and SVG parsing remain unfinished APIs; bounded
single-polygon/polyline rasterization is documented below.
Adaptive flattening uses the separate flatten_cubic/flatten_path calls below. Native
API01 curve/editor and Draw/Paint behavior coexist.

Native input/output vectors reserve the shared native quota before allocation. Parsing,
evaluation and Lua-column output check cancellation at most64 items apart; each cubic
also checks when it finishes. No Lua callback executes inside a native evaluator or
on a render/audio thread. Native buffers are released on success, error or cancellation;
returned plain tables use the VM quota. No project permission is needed for these math
operations. Editing notes still requires the usual transaction/capability/lock rules.

[Geometry Primitives](../crates/plumspirit/examples-lua/geometry-primitives/README.md)
includes five explicitly authored Lua commands: triangle, Bézier, fixed mixed path,
adaptive polygon mask and adaptive mixed path.
Their deterministic replay, private Preview/discard, Apply and native Undo/Redo are
tested. A separate actual Windows worker smoke verifies the math without project-read
permission. This does not claim an interactive graph/drawing interface or final release
acceptance. Native Custom Shape output digests match the pre-extraction baseline for
40 seeded fixtures across Standard and256-Key modes.


<a id="adaptive-cubic-flattening-and-geometry-predicates-api-02-development"></a>

## Adaptive cubic flattening and geometry predicates (API 0.2)

These are pure worker-side mathematical services with no project permission, Lua
callback, rendering surface or retained native resource. API 0.1 `curve`, its native
editor, and native Draw/Paint remain supported. All point inputs use the same plain,
dense `{count?,x,y}` column schema, finite local coordinates within +/-1e12 and at most
4096 points. Outputs are independent Lua-owned data. Musical edits still require the
existing capabilities, target/lock checks and transaction/Preview/Apply path.

| Call | Implemented behavior |
| --- | --- |
| `geometry.flatten_cubic(control,options)` | Adaptively flattens exactly four cubic controls into `{count,x,y}`. Ordered endpoints are exact authored first/last controls. |
| `geometry.max_flatten_depth` | Constant24. |
| `geometry.nearest_polyline(points,x,y,options?)` | Returns `{x,y,distance,segment,t}` for the nearest finite line segment; segment is1-based and t is in[0,1]. |
| `geometry.polygon_classify(points,x,y,options?)` | Returns `"inside"`, `"outside"` or `"boundary"` for a single implicitly closed polygon. |

Flattening requires `options.tolerance`, a numeric finite positive Euclidean distance
in the input coordinate units, at most1e12. Optional `max_points` is an integer2–4096
(default4096); `max_depth` is an integer0–24 (default24). De Casteljau subdivision accepts
a subcurve only when its two inner controls lie sufficiently close to its **finite**
endpoint chord. The convex control hull bounds deviation, including loops, inflections,
coincident endpoints and collinear backtracking which a midpoint-only test could miss.
A conservative arithmetic allowance of128 times machine epsilon times the largest
absolute input coordinate (minimum scale1e-300) is subtracted from tolerance. A tolerance
at or below that allowance fails explicitly; use local offsets for large song origins.
Calculations use IEEE f64; this is not an exact arithmetic geometry kernel.

If the requested tolerance cannot be met at a depth or point limit, the call fails
with a Budget error naming `max_depth` or `max_points`. It never silently returns a
coarser successful result. Invalid tolerance/coordinate precision is a Runtime error.
The original input is unchanged and no partial output is returned. Native quota is
reserved for the maximum requested output and fixed stack before allocation; VM output
also uses the existing VM limit. Iterative bounded work checks cancellation every64
visited nodes and before completion. Point count includes both endpoints, so an exactly
constant or accepted straight cubic still returns two points. Repeated calls with the
same inputs produce the same ordered samples in the same host. Existing `sample_cubic`
and `sample_path` continue to use fixed parameter sampling; their behavior is unchanged.

```lua
local g=plum.geometry
local controls={x={0,1,2,3},y={0,2,-2,0}} -- authored inflection
local polyline=g.flatten_cubic(controls,{tolerance=0.001,max_points=2048,max_depth=24})
local spaced=g.resample_polyline(polyline,32)
local hit=g.nearest_polyline(polyline,1.5,0.2)
-- hit.segment identifies a polyline edge, not an original Bézier parameter.
local mask={x={0,3,3,0},y={-1,-1,1,1}}
local classification=g.polygon_classify(mask,spaced.x[10],spaced.y[10],
                                      {rule="even_odd",epsilon=0.00001})
```

Nearest-polyline options contain only `closed` (boolean, defaultfalse). Closing adds
an edge from the last point to the first; no sample is added to the input. A one-point
polyline acts as a zero-length segment1 with t=0. Zero-length edges are valid. Equal
computed distances choose the earliest edge. Distance and t are measured in the supplied
coordinate space, so apply the shared affine mapping first for screen-space hit tests.
This is an O(n) query on at most4096 points, not a project-wide note spatial index.

Polygon options contain `rule` (`"even_odd"` default, or `"non_zero"`) and `epsilon`
(default0, finite nonnegative distance at most1e12). At least three points are required.
Native area-engine half-open ray crossings count vertices once and implement the fill
rule, including self-intersections/repeated winding. Within epsilon of any finite edge
returns `"boundary"` before fill classification; epsilon0 uses the computed zero
segment distance. Repeated vertices and an explicitly repeated closing vertex are
accepted. Degenerate polygons can classify on-edge points as boundary, otherwise outside.
Multi-contour/hole/Boolean regions remain unavailable; the separate raster APIs below
provide bounded even-odd fills and native one-cell strokes. Near floating-point edges choose an epsilon appropriate to the author's domain.
Polygon and polyline scans check cancellation every64 edges; input reservations drop on
success, failure or cancellation. Unknown fields, metatables, holes, invalid coordinates,
noninteger limits and excessive columns are refused by the shared strict parser.

The installable Geometry Primitives example includes `Generate Author Adaptive Mask`.
Its Lua code authors the curve, mask, pitch reflection and distance-driven velocity.
Host integration executes the actual entry point, discards private Preview, replays
identically, applies once and verifies native Undo/Redo. This does not yet provide
interactive graph UI, drawing tools or full API 0.2 release acceptance.


<a id="adaptive-mixed-path-flattening-api-02-development"></a>

## Adaptive mixed-path flattening (API 0.2)

`plum.geometry.flatten_path(path,options)` returns ordered `{count,x,y}` columns for
a mixed line/cubic path. It uses the same strict descriptor as `sample_path` and the
same native adaptive cubic kernel as `flatten_cubic`; no Lua evaluator callback or
project permission is required. There is no arbitrary SVG, spline or multi-contour
parser. The author supplies every segment and mathematical control point.

```lua
local g=plum.geometry
local path={start={x=-2,y=0},closed=true,segments={
  {kind="line",x=0,y=0},
  {kind="cubic",x1=1,y1=2,x2=2,y2=-2,x=3,y=0},
  {kind="line",x=3,y=-1},
}}
local points=g.flatten_path(path,{tolerance=0.001,max_points=2048,max_depth=24})
local spaced=g.resample_polyline(points,32)
-- The Lua author maps these arbitrary local coordinates to ticks/pitches/velocities.
```

`path` contains only `start={x,y}`, a dense plain `segments` array (0–256 entries), and
optional boolean `closed` (defaultfalse). A segment is either `{kind="line",x,y}` or
`{kind="cubic",x1,y1,x2,y2,x,y}`. All coordinates are finite within +/-1e12. A cubic
starts at the preceding segment's exact endpoint. The initial point is emitted once;
each line appends its exact endpoint and each cubic appends adaptive samples excluding
its shared start. Authored repeated/degenerate endpoints remain. With `closed=true`,
the original start is appended as a straight closure only if the final endpoint differs.
Empty paths return one point even when closed; an already closed path adds no extra
point. No approximation or simplification is applied to line endpoints or closure.

Options contain required numeric finite positive `tolerance` (at most1e12), optional
integer `max_points`1–4096 (default4096), and optional integer `max_depth`0–24
(default24). Tolerance is the shared Euclidean control-hull bound in authored coordinate
units, including S07C's conservative128-epsilon allowance. The depth limit applies to
each cubic. Precision-floor validation applies to each actual cubic; line-only/empty
paths do not need subdivision precision and accept any otherwise valid tolerance.
Unlike `flatten_cubic`, an empty path can use max_points=1. `curve_steps` is not an
adaptive option. Unknown fields, wrong kinds/types, metatables, holes, nonfinite or
out-of-range coordinates and excessive segments fail through the shared parser.

`max_points` bounds the **entire path**, including its initial point, every retained
joint, all cubic samples and any closing point. The host preflights the minimum count
before allocating output, and reserves one endpoint for each remaining segment and
for required closure before flattening each cubic. Remaining slots are passed to the
unchanged native kernel. It never silently coarsens a segment to fit: if tolerance
cannot be met within remaining points/depth, a Budget error names max_points/max_depth.
A cubic below coordinate precision fails with Runtime. A late failure or cancellation
discards the complete private path; no partial table or project edit is returned.

Curved paths preflight the simultaneous final output, one native cubic scratch vector,
fixed subdivision stack and parsed input. Each vector is at most max_points; only one
cubic scratch exists at a time and it is dropped after copying bounded native points.
There are no intermediate Lua point tables. Line-only/empty paths reserve their exact
output count and need no cubic scratch/stack reservation. Native reservations release
on every exit; returned columns use the VM quota. Processing checks cancellation at
every segment, every64native nodes/copied points, before completion and during output.
Pure math does not bypass existing transaction permissions, locks or Preview/Apply rules.

Geometry Primitives now includes `Generate Author Adaptive Path`: an explicitly
Lua-authored mixed path, adaptive flattening and arc-length note placement. All five
commands execute in host integration with private Preview/discard, deterministic replay,
one-step native Apply, Undo and Redo. Actual Windows worker evidence is recorded
separately from these source/integration checks. Custom graph windows, pointer drawing
interfaces and full API 0.2 release acceptance remain separate required development.


<a id="polygon-and-path-note-cell-rasterization-api-02-development"></a>

## Polygon and path note-cell rasterization (API 0.2)

`plum.geometry.rasterize_polygon(points,grid)` and
`plum.geometry.rasterize_polyline(points,grid,options?)` reuse the actual native
Draw/Paint pen's scanline, stroke merging and grid quantization. They are pure worker
math: no project permission, callback, note insertion or new note-color storage. Lua
supplies the shape and determines how returned cells map to MIDI or masks.

Input uses the existing plain dense `{count?,x,y}` schema, at most4096 finite points
within +/-1e12. Polygon fill needs at least3points and closes implicitly. Polyline
stroke accepts1point and optional `{closed=true}`; an extra closing point is added only
when endpoint coordinates differ. Unknown fields/metatables/holes/invalid coordinates
are refused. Arbitrary curves/paths compose through `flatten_path`:

```lua
local g=plum.geometry
local path={start={x=0,y=0},closed=true,segments={
  {kind="line",x=8,y=0},
  {kind="cubic",x1=12,y1=2,x2=4,y2=6,x=0,y=4},
}}
local points=g.flatten_path(path,{tolerance=0.001})
local spans=g.rasterize_polygon(points,{columns=16,rows=8})
-- Each row/first/last is an integer grid index, not an absolute tick or pitch.
for i=1,spans.count do
  local row=spans.row[i]
  for column=spans.first[i],spans.last[i]-1 do
    -- The author chooses timing, gate, pitch, velocity, Track and Subchannel.
  end
end
```

Grid fields:

| Field | Meaning |
| --- | --- |
| `columns` | Required Lua integer1–65536; horizontal cells in the clipped viewport. |
| `rows` | Required integer1–4096; vertical cells in the clipped viewport. |
| `origin_x`, `origin_y` | Finite local input coordinates at grid index(0,0), default0. |
| `cell_width`, `cell_height` | Finite positive input coordinate sizes, default1, at most1e12. |
| `max_spans` | Integer1–65536, default8192; raw clipped pieces allowed before union. |

Normalization is `(x-origin_x)/cell_width`, `(y-origin_y)/cell_height`. Normalized
coordinates must also be finite within +/-1e12. Use local origins/offsets for precision;
this f64 geometry service does not silently convert large absolute song ticks.
`plum.ticks` checked integer arithmetic should add musical origins after rasterization.
The author chooses row-to-pitch mapping, including128/256-Key projects. Grid rows are
not implicitly limited to a keyboard; musical transactions enforce actual project keys.

Results contain `{count,cell_count,row,first,last}`. The three columns have count entries,
sorted by row then first; row and first are zero-based, last is exclusive. Overlapping
or adjacent intervals on a row are united. Thus each covered cell appears once and
cell_count is the exact sum of(last-first), potentially much larger than count. The
result is compact spans, not a Lua table per cell. Empty geometry returns count0 and
empty columns. All returned spans lie inside[0,columns) × [0,rows).

Fill is **even-odd**, sampled at each row centre(row+0.5), with the native half-open
vertex crossing rule. Horizontal endpoints round to the nearest grid boundary (half
values away from zero). A positive thin interval spanning no rounded cells uses the
native minimum-one-cell fallback at its midpoint. A vertically thin fill crossing no
row centre produces no cells. Repeated winding can cancel under even-odd fill.
`polygon_classify` also supports nonzero winding, but this rasterizer currently exposes
only native even-odd fill; it does not imply nonzero/multi-contour/Boolean region APIs.

Strokes use the native pen's row traversal and merging before quantization. Horizontal
segments produce row spans; steep segments produce one-cell pieces. A single point
acts as a minimum-one-cell stroke. Shared corners can differ from independently rounded
line segments because native consecutive pieces merge before rounding; this preserves
existing Draw/Paint behavior. Rasterization does not offer brush thickness/cap/join
settings in this slice. Authors can rasterize their own sampled outlines/filled shapes.

Work is preflighted at most4194304vertex×grid-row tests (including an appended stroke
closure); crop the viewport or split the shape for larger requests. Native y clipping
and x interval clipping avoid scanning individual cells across the horizontal extent.
Raw clipped pieces are bounded by max_spans **before union**; heavily overlapping
requests can fail even if the final union would be small. Budget failures return no
partial result. Maximum constants are exported as max_raster_columns, max_raster_rows,
max_raster_spans and max_raster_work. Native quota reserves input, normalized coordinates,
fill crossings/stable-sort scratch and the full requested span capacity before allocation. Returned columns
use the Lua VM quota; native buffers release on success/error/cancellation.

Native fill checks cancellation per row/every64vertices, including empty-output rows;
strokes check per segment/every64traversed rows. Normalization/union/column emission check
every64items; sorting is a single bounded native phase. Failed/canceled work stays
private. The geometry result creates no notes: existing writer/transaction output,
confirmation, eligibility, lock and Undo budgets still apply to musical edits.

Geometry Primitives includes `Generate Author Raster Cells`, an explicitly authored
11×3polygon whose33cells are streamed through the transaction-owned note writer.
The Lua code chooses tick spacing, gate, velocity and the top3project pitches. Host
integration runs all six actual commands in both128/256-Key projects, with private
Preview/discard, deterministic replay/Apply and native Undo/Redo. Native generator
before/after digests and real Windows worker probes are recorded separately; no full
interactive graph/drawing or release compatibility claim follows from pure math tests.


<a id="bounded-font-resources-and-scalar-metrics-api-02-development"></a>

## Bounded font resources and scalar metrics (API 0.2)

The existing native skrifa font stack now backs these worker-side calls in `plum.text`.
They create job-scoped font snapshots, not OS paths or executable extensions:

| Call | Implemented behavior |
| --- | --- |
| `text.builtin_font(kind?)` | Load the app's existing licensed bundled `"proportional"` default, `"monospace"` or `"emoji"` face; no permission or project needed. |
| `text.load_font(selected_file,face_index?)` | Read an approved read-only file handle from offset0, preserving its script cursor; face index defaults0, integer0–255. |
| `font:info()` | Independent metadata table: source, source_name, face_index, bytes, units_per_em, glyph_count, outline_format, ascent, descent, line_gap, line_height. |
| `font:glyph_id(value)` | Map exactly one UTF-8 Unicode scalar string or integer codepoint to its actual font glyph ID; missing/.notdef mapping returns nil. |
| `font:glyph_advance(id)` | Advance in em units for explicit integer glyph ID0–glyph_count-1;0 is the font's explicit .notdef glyph. |
| `font:close()` | Release font bytes now; returns true once, false when already closed/expired. |

`source_name` is the actual bundled font key or selected/package file's short filename. It is
not a claim about an installed family name. The API does not enumerate installed fonts
or accept arbitrary filename strings. External files still require
`file.read_user_choice` and the user-approved dialog; a selected handle can be closed
after loading because the font owns its snapshot. The same file can supply multiple
collection faces while keeping its original read offset. Raw sfnt TTF/OTF/TTC parsing
uses skrifa; tests cover bundled TTF assets, malformed data and a two-face TTC assembled
from those licensed assets. Other containers such as WOFF/WOFF2 are not exposed here.
Invalid/unavailable faces fail explicitly instead of silently falling back.

Metrics except units_per_em/glyph_count/bytes are normalized **em units**, with font
Y positive upward and signed descent preserved. Glyph IDs are face-specific integers,
not Unicode character codes. A space may have a valid advance without visible outline.
Coverage is deliberately a **single-scalar query**, not shaping. Multi-scalar graphemes,
ZWJ sequences and combining clusters are refused by glyph_id; use the existing grapheme
API for segmentation, and the later shaping API for rendered clusters. Missing input
is not replaced by '?' and the original UTF-8 is never modified. Coverage itself does not claim complex-script shaping or full text layout. Bounded
original glyph outlines and a monochrome emoji face are available separately below.
The existing native Spiderweb text/font behavior remains unchanged.

```lua
local proportional=plum.text.builtin_font()
local monospace=plum.text.builtin_font("monospace")
local glyph=proportional:glyph_id("é")
if glyph then local advance=proportional:glyph_advance(glyph) end
proportional:close();monospace:close()

local file=plum.files.open{filters={{name="OpenType",extensions={"ttf","otf","ttc"}}}}
if file then
  local font=plum.text.load_font(file,0)
  file:close()
  local metadata=font:info() -- retain plain metadata if needed, never the live handle
  font:close()
end
```

Limits: input1byte–64MiB, collection1–256faces, at most32created handles per job
(including already-closed handles). Constants max_font_bytes/max_font_faces/
max_font_handles are exported. Native quota preflights font bytes, load scratch and
64KiB copy/read chunks. Metadata/coverage/advance calls reserve bounded parser workspace
and check cancellation before/after native work. Font-wide metadata does not draw glyph
outlines as a hidden bounds/cap-height fallback. Host cleanup eagerly releases every
font's byte buffer on completion, failure or cancellation. Weak job references prevent
retained userdata keeping an active job alive. Each retained font userdata keeps a
512byte native metadata reservation until Lua collects it, even after close/expiry;
thus expired-handle accumulation remains accounted. Both native and Lua VM quotas apply.
A retained handle cannot be used in a later job; load a new snapshot. Presets can store
plain choices/metrics, but live font userdata and approved file handles are not persistent.

Install [Font Resource Inspector](../crates/plumspirit/examples-lua/font-resources/README.md).
Its real commands compare bundled faces or inspect an approved selected font, report
scalar coverage/advances and save only plain report data. A notification names the
resource; it creates no musical edits. Headless real-file I/O tests are distinct from
actual Windows picker/creative font selector acceptance. Installed-face discovery,
outlined/shaped typography, UI selection/presets and Text Art Studio remain required
future development; this resource foundation alone does not complete TEXT-01/02/UNI-01.


<a id="original-cubic-glyph-outlines-api-02-development"></a>

## Original cubic glyph outlines (API 0.2)

`font:glyph_outline(glyph_id,options?)` returns actual original cubic contours in em
units using the same native Spiderweb/skrifa converter as existing font tools. Quadratic
segments are raised to cubics and line segments use collinear cubic handles; this is
not a generated approximation or a new host-authored art algorithm. The caller supplies
an explicit face-specific glyph ID, normally from glyph_id or a future shaping result.
Missing scalar coverage still returns nil; a malformed cmap pointing outside the font's
glyph count now fails explicitly. No silent '?' fallback is added to this public API.
Existing native v1 font fallback and curve conversion remain unchanged.

The compact result is:

| Field | Meaning |
| --- | --- |
| `count` | Total original cubic control points, including each contour's closing endpoint. |
| `contour_count` | Number of contours. |
| `x`, `y` | Dense1-based point columns in em units, Y upward. |
| `first`, `last` | Dense contour ranges: first is1-based inclusive; last is1-based exclusive. |
| `advance` | Advance in em units, independent of outline width. |
| `units`, `format` | `"em"`, `"cubic"`. |
| `status` | `"outline"` or `"no_outline"`; blank glyphs can have a positive advance. |
| `control_bounds` | Optional `{x_min,y_min,x_max,y_max}` of controls, not an exact ink box. |

Each contour begins with one point, then triples(control1,control2,end). Its point count
is3n+1 and the final endpoint equals the start. Thus range length last-first is3n+1.
Original points remain independent mutable Lua data; editing them does not modify the
font. A no-outline result contains count0/contour_count0 and empty columns with no
control_bounds. It means no visible outline is available, not that a scalar is missing.
Do not assume every covered glyph is visible: whitespace and other glyph technologies
can have no monochrome outline.

```lua
local font=plum.text.builtin_font("proportional")
local glyph=font:glyph_outline(assert(font:glyph_id("O")))
for c=1,glyph.contour_count do
  local start,finish=glyph.first[c],glyph.last[c]
  local x,y=glyph.x[start],glyph.y[start]
  for i=start+1,finish-1,3 do
    local cubic={x={x,glyph.x[i],glyph.x[i+1],glyph.x[i+2]},
                 y={y,glyph.y[i],glyph.y[i+1],glyph.y[i+2]}}
    local points=plum.geometry.flatten_cubic(cubic,{tolerance=0.001,max_points=256})
    -- The author appends/maps/rasterizes samples and decides the musical meaning.
    x,y=glyph.x[i+2],glyph.y[i+2]
  end
end
font:close()
```

Options are plain tables with only integer max_points4–65536 (default4096) and
max_contours1–256 (default64). The cap includes emitted closure controls; exhausted
limits fail with Budget and return no partial contours. Constants max_glyph_points,
max_glyph_contours and max_glyph_draw_bytes(16MiB) are exported on plum.text. Native
quota preflights point/contour growth and the exact native draw scratch before allocation;
the decoder receives that explicit scratch buffer. Finite coordinates within +/-1e12
are required. Native cancellation is observed at collector callbacks and before/after
draw, with the decoder's bounded glyf point/recursion phase completing safely; no Lua
callback executes inside it. Lua output checks every64points. Results use VM quota.
Glyph data over geometry's4096point batch limit can be processed in bounded chunks.

`font:info().outline_format` reports glyf/cff/cff2/varc/none. This API currently exposes
**unhinted glyf** geometry (including default-instance variable glyf), with the native
64Kpoint/recursion bounds. It executes no font hinting program. CFF/CFF2/VARC and
bitmap-only geometry fail clearly as unsupported here: a subroutine depth limit alone
is not an adequate total-work budget for plugin CFF drawing. Font metrics/loading can
still be available for those faces. No full arbitrary-format/variable-axis/color-font
or complex-script/ZWJ shaping claim is made. The existing native font tool retains its
previous formats and behavior. A bounded route for remaining supported technologies
remains required development before broader acceptance.

`plum.text.builtin_font("emoji")` selects the app's existing licensed bundled Noto emoji
face. It yields real monochrome outlines for supported glyphs such as 😀; it does not
promise every emoji sequence or color representation. No extra font files or proprietary
fonts are bundled by these examples.

Install [Text Art Studio](../crates/plumspirit/examples-lua/text-art-studio/README.md).
Its original single-face composition choices remain. New explicit Proportional + Emoji
and Monospace + Emoji modes demonstrate author-controlled whole-grapheme fallback,
visual subrun order and face-local outline caches. Mixed `AV😀A` / `π🔥` and `🇺🇸`
produce real glyph artwork; the sample rejects unsupported modifier/family/keycap
composition while preserving text and source notes. The existing native parameter
presets/Preview/Apply/Cancel/one Undo/Redo apply to these choices as well. This is an
authored bundled-font policy, not universal font fallback or color rendering.

Its original outline/fill/shaped commands are explicit Lua algorithms: font/scalar selection,
adaptive curve sampling, orientation, clipped stroke rasterization, cell union and
streamed MIDI mapping. Tests execute all six in128/256-Key projects, verify two fonts
produce different actual note hashes, private Preview/discard, deterministic Apply and
native Undo/Redo. Shaped multiline composition and explicit bundled fallback are available above.
Integrated installed-font typography, broader contextual layout and final rich color
artwork acceptance remain in development.

<a id="multi-contour-cell-regions-api02-development"></a>

## Multi-contour cell regions (API 0.2)

`plum.geometry.rasterize_region(contours, grid, options?)` fills developer-authored
contours together. It reuses the native area scanner and Pen quantization; no notes
are created. `contours` is a plain dense1-based array of1–256 point descriptors
`{x={...},y={...},count?}`. Each needs at least3points; **4096total vertices** across
all contours is the limit. Each contour closes automatically; a repeated endpoint is
accepted and counts toward the limit. Existing polygon/polyline APIs are unchanged.

The grid and output schema are exactly those of rasterize_polygon: dimensions,
origin/cell scale, max_spans, compact sorted unique half-open row spans and exact
cell_count. Grid rows are sampled at row+0.5. X crossings round using the existing
native Pen rule, including its minimum-one-cell fallback for thin intervals. Adjacent
rounded spans unite; sub-cell holes may disappear at insufficient resolution.

Options is a plain table containing only `rule="even_odd"` (default) or `"non_zero"`.
Even-odd uses combined crossing parity; repeated identical contours cancel. Nonzero
uses signed winding; an opposite-oriented inner contour creates a hole. Equal-oriented
nested contours remain filled. There is one combined region, not a union of separate
polygon fills. The plugin controls orientations and sampling tolerance.

```lua
local outer={x={0,8,8,0},y={0,0,8,8}}
local inner={x={2,6,6,2},y={2,2,6,6}}
local cells=plum.geometry.rasterize_region({outer,inner},{columns=8,rows=8})
assert(cells.cell_count==48) -- the4x4 hole remains empty
-- Interpret cells as notes, an image mask, selections or other author-defined data.
```

Native work is capped at4194304total-vertex×row tests, as for polygon rasterization.
The native fill-only path performs no boundary-intersection pair expansion. Native
quota covers retained contours/build/row buffers and capped span capacity before
allocation; results use the VM quota. Cancellation is checked between bounded contour/
build/row phases, every64copied points and output intervals. A native build/row phase
has at most4096edges; no Lua callback runs inside it. Exceeding any limit or cancellation
publishes no partial result. Raw clipped emitted spans are capped before output union.
`max_region_contours` is256; existing raster constants still apply.

Text Art Studio's filled modes explicitly sample original font cubic contours in Lua
then rasterize them together using nonzero winding. A real O-hole regression verifies
that separate polygon fills would incorrectly paint the counter. All six real command
entry points retain private Preview/discard, replay/Apply and native Undo/Redo in both
key modes. These headless integration tests do not establish custom UI acceptance.

<a id="color-glyph-layers-api02-development"></a>

## Color glyph layers (API 0.2)

`font:color_info()` returns nil without a COLR table, or a plain description of the
implemented **COLRv0 layer route**: `format="colr_v0"`, actual `table_version` (0 or1),
`palette_count`, `palette_entries`, `base_glyph_count`, total `layer_count` and
`v0_layers_available`. A version1 COLR table can contain original v0 compatibility
records. This method does not execute or advertise COLRv1 paint graphs. Counts are
metadata, not a promise that every glyph or palette record can be decoded.

`font:color_layers(glyph_id[,options])` returns the original ordered v0 layers for a
valid0-based glyph index, or nil when there is no COLR/v0 base record for that glyph.
Nil does not imply the glyph has no other representation. Options are a strict plain
table: `palette` (0-based integer, default0), `max_layers` (integer1–256, default64),
`foreground` (plain four-byte integer RGBA array, default `{0,0,0,255}`).
`plum.text.max_color_layers` is256. CPAL versions0/1 share the implemented palette
colors; version1 palette labels/types and variable/color paint behavior are not exposed.

The result is plain `format="colr_v0"`, actual `table_version`, selected `palette`,
`palette_count`, `count` and equal packed columns `glyph_id`, `palette_index`,
`foreground`, `red`, `green`, `blue`, `alpha`. Layer order is preserved, bottom to top.
RGBA components are straight integer bytes0–255; CPAL's stored BGRA order is converted
explicitly. Palette index65535 means use the author-supplied foreground, with
`foreground=true`; other colors come from the selected CPAL palette. Transparency
and zero-alpha RGB are preserved. No per-note color field is created.

Layer ranges, glyph IDs, selected palette indices and color-record ranges are checked.
A result exceeding max_layers fails rather than truncating. Native layer workspace
is reserved before allocation, output uses the VM quota, and scan/output checkpoints
preserve cancellation/deadline errors. Invalid/corrupt/unsupported table data and
options use `runtime`; layer/native limits use `budget`; allocation/VM refusal uses
`memory`. No partial layer set, mutable font data, paths or native addresses are
returned. Same-job Font scope, installed fonts.list grants/revocation and eager
cleanup apply to both methods, including a metadata-only query.

This supplies original layer glyphs/colors for plugin-authored geometry, masks,
custom canvas presentation and musical interpretation. Each layer's actual outline
support remains independent: the current bounded glyph_outline route requires glyf.
CFF/CFF2/VARC, COLRv1 paints/gradients/composites, SVG and bitmap glyph execution are
not implemented by this layer API. Proprietary installed fonts remain user resources;
they are not bundled. Notes retain native Track/Subchannel colors: authors explicitly
map artwork colors through supported Track/Subchannel mechanisms.

```lua
local function inspect_color(font,scalar)
  local glyph=assert(font:glyph_id(scalar),"Missing scalar; preserve original text.")
  local layers=font:color_layers(glyph,{palette=0,max_layers=256,
    foreground={20,30,40,255}})
  if not layers then return nil,"No COLRv0 layers for this glyph." end
  assert(font:info().outline_format=="glyf","This example requires bounded glyf contours.")
  local points=0
  for i=1,layers.count do
    local contour=font:glyph_outline(layers.glyph_id[i],{max_points=4096,max_contours=64})
    points=points+contour.count
    -- Author chooses geometry transforms, joint contour fill/A8 masks, preview
    -- color and native Subchannel routing. Color bytes are layers.red[i], etc.
  end
  return layers,points
end
-- Supply a Font from your explicit authorized selection/catalog or package asset.
```

Text Art Studio's installable **Color Glyph Laboratory** command is a complete
Lua-authored composition of this route: explicit installed family/style/single
grapheme/palette, isolated shaping, original v0layer glyphs, one common transform,
joint contour fill, A8/colorize/native composition and one native canvas image.
It keeps Font handles out of the panel wait loop. Typed resize events preserve
square aspect without changing glyph geometry. Save inputs/presets only plain data;
failed rendering retains the previous image and original input, and no musical
transaction/Undo is created. The command separately requests fonts.list/ui.panels,
preserving the original art commands/tool permissions. Limits are31layers,64pixel
resolution,4catalog/render attempts, existing64cumulative image/mask creations and
16UIimages; no host limit is raised. See its README for cancellation, persistence
and exact format/presentation limits. Native Subchannel MIDI routing follows as a
separate authored tool; this panel alone changes no notes.

Tests use manufactured COLR/CPAL tables over real licensed bundled glyf outlines and
an actual configured Windows Segoe UI Emoji catalog load with original v0-compatible
layer outlines. That verifies this extraction route, not full interactive colored
artwork or every installed font/color technology.

<a id="font-coverage-runs-api02-development"></a>

## Font coverage runs (API 0.2)

`plum.text.font_coverage_runs(text,fonts[,options])` is a deterministic first step
for an authored fallback system. `fonts` is a strict dense array of1–8already-loaded
Font handles in the plugin's explicit priority order. For each whole extended
grapheme, native cmap lookup chooses the first font mapping every required scalar
to a valid nonzero glyph. A later font cannot override an earlier complete match.
Partial coverage from several fonts is never combined inside a grapheme. No font
is discovered, opened, granted or silently substituted. Every supplied handle is
validated even if unused; closed/expired/revoked installed fonts fail normally.

The source is valid UTF-8, at most16,384bytes and4,096scalars, as for native layout.
Original bytes, hard separators and grapheme boundaries remain intact; no Unicode
normalization, replacement or automatic control removal is performed. Options are
plain, accepting only `max_runs` (integer1–4096, default4096) and optional
`ignored_codepoints` (a plain dense array of at most32distinct Unicode scalar
integers, default empty). An explicit ignored scalar is excluded only from the
coverage decision; it is still present in the original text and later shaping.
For example, authors may explicitly ignore ZWJ/variation selectors when testing
component coverage. Such an exclusion does **not** establish support for their
requested composition, variation, color or presentation.

The result is plain `text`, `count`, `font_count`, `grapheme_count`,
`covered_graphemes`, `missing_graphemes`, `ignored_graphemes`,
`ignored_scalar_count` plus equal packed columns:

| Column | Meaning |
| --- | --- |
| `first`, `last` | Original1-based inclusive/exclusive UTF-8 byte range; adjacent matching selections coalesce, without splitting graphemes. |
| `font` |1-based index into this call's font list, or0when no font was selected; not a persistent font ID. |
| `status` | `covered` if a font maps every required scalar, `missing` if none does, `ignored` if the entire grapheme was explicitly excluded. |
| `graphemes` | Number of whole graphemes in the coalesced run. |
| `ignored` | Number of explicitly excluded scalars inside that run. |

Missing and fully ignored runs retain their text with `font=0`; the host does not
invent a replacement appearance. Empty text gives zero runs but still validates
the font list. Changing returned tables cannot change fonts or later results.
This service is cmap coverage selection, not shaping, font-layout itemization,
normalization equivalence, variation-sequence verification or a guarantee of a
composed emoji. Bundled family emoji can map every component yet remain uncomposed;
`shape_run` and the actual outline/raster support must be checked separately.

Existing layout native preflight and font parser scratch are charged before their
buffers; no font bytes or glyph arrays are copied into the result. Source/font scans,
coalescing and output check cancellation. Invalid descriptors/UTF-8/handles produce
normal `runtime`/`expired`/`capability` errors; dimension/list/run/native quotas use
`budget`, allocation/VM failures use `memory`. Failure returns no partial run set,
no transaction and no newly selected font resource. Constants are
`max_coverage_fonts=8`, `max_coverage_runs=4096`, `max_coverage_ignored_codepoints=32`.

```lua
plum.require_api(0,2)
local t=plum.text
local fonts={t.builtin_font(),t.builtin_font("emoji")}
local source="A😀A"
local coverage=t.font_coverage_runs(source,fonts)
assert(coverage.text==source and coverage.count==3)
assert(coverage.font[1]==1 and coverage.font[2]==2 and coverage.font[3]==1)
assert(coverage.missing_graphemes==0)
-- Author decides run order/layout/limits. Keep logical text for native shaping.
for i=1,coverage.count do
  assert(coverage.status[i]=="covered")
  local text=source:sub(coverage.first[i],coverage.last[i]-1)
  local shaped=fonts[coverage.font[i]]:shape_run(text,{direction="ltr"})
  assert(shaped.text==text and shaped.missing_count==0 and shaped.count==1)
  assert(fonts[coverage.font[i]]:glyph_outline(shaped.glyph_id[1]).count>0)
end
for _,font in ipairs(fonts)do font:close()end
```

For general text, compose explicit hard lines, bidi/script runs, coverage selection
and shaping within their respective documented limits. Respect shaping's256scalar
single-run limit and contextual/unsafe-break information; this coverage primitive
does not promise correct complex-script layout across arbitrary author-chosen cuts.

<a id="shaped-glyph-runs-api02-development"></a>

## Shaped glyph runs (API 0.2)

`font:shape_run(text, options?)` uses the app's existing Harfrust0.12 shaping engine.
Bundled fonts use the trusted native worker route. On Windows, user-selected,
installed and private package fonts use a host-owned resource-isolated process running
the same native shaper. `font:info().shaping_supported` reports backend availability;
`shape_backend` is `"bundled_native"`, `"isolated_native"` or `"unsupported"`. Availability
does not guarantee coverage or successful shaping of every font. No new font binary,
download or shaping algorithm is introduced.

A run contains at most4096UTF8bytes and256Unicode scalars. It has one script/direction/
language, with no CR/LF or Unicode line/paragraph separators. This is not paragraph
bidi resolution, wrapping, fallback or multiline layout. The caller splits those runs;
`direction="auto"` guesses native segment properties, not a whole mixed bidi paragraph.

Options are a plain table containing only:

| Option | Meaning |
| --- | --- |
| `direction` | `"auto"` default, `"ltr"`, `"rtl"`, `"ttb"` or `"btt"`. |
| `script` | Optional four-letter ISO15924 tag such as `"Latn"`; otherwise native guess. |
| `language` | Optional1–63ASCII alphanumeric/hyphen language tag, such as `"en"`. |
| `features` | Optional plain map of up to16four-character OpenType tags to boolean or integer0–65535; global to this run, e.g. `{liga=false,kern=true}`. |
| `max_glyphs` | Integer1–4096, default4096; overflow returns no partial run. |

The result preserves the original `text` exactly and contains `count`, `units="em"`,
resolved `direction`, `script`, optional `language`, `advance_x`, `advance_y` and
`missing_count`. These dense1-based columns each have count elements:

| Column | Meaning |
| --- | --- |
| `glyph_id` | Font glyph index for glyph_outline;0is the explicit missing glyph. |
| `first`, `last` | Original UTF8 byte cluster range:1-based inclusive first, exclusive last. Several glyphs can share a cluster; ligatures can merge adjacent clusters. |
| `x`, `y` | Glyph drawing origin in em units, Y upward, including its offset. |
| `x_advance`, `y_advance` | Pen movement after the glyph; sum to advance_x/y. |
| `x_offset`, `y_offset` | Position adjustment before drawing, independent of advance. |
| `missing` | True when glyph_id0; no silent question-mark substitution. |
| `unsafe_to_break` | Native shaping flag; preserve cluster/run context when splitting. |

Input scalar cluster tags come from the existing UAX29 grapheme segmentation, so a
ZWJ family, modifier, flag or keycap sequence retains one original text cluster even
when a font lacks its composed appearance. A single cluster does not guarantee one
glyph or a supported appearance. Inspect missing flags and the actual glyph geometry.
The shaper may additionally merge clusters for ligatures. Output ordering follows the
resolved direction; cluster byte offsets can descend for RTL/BottomToTop.

```lua
local f=plum.text.builtin_font()
local run=f:shape_run("office é",{script="Latn",language="en"})
assert(run.missing_count==0)
for i=1,run.count do
  local glyph=f:glyph_outline(run.glyph_id[i])
  -- Map glyph.x/y by run.x[i],run.y[i], then use native geometry/raster APIs.
  -- Whitespace can have no outline while still advancing the pen.
end
f:close()
```

Bundled shaping reserves32MiB of transient native quota for buffers, growth and known
font caches/plan/scratch. External shaping reserves128MiB plus416KiB of native quota
for the isolated process and bounded parent protocol/validation workspace, in addition
to the already loaded font. These are accounting reservations, not eager allocations.
A256MiB plugin budget can therefore refuse shaping when other resources are retained.

The Windows helper is the same executable in a fixed stdin-only internal mode, before
GUI/settings/audio startup. It receives authorized font bytes, validated options and
text; it has no plugin callbacks, paths, arbitrary executable choice or exposed process
API. The host attaches an unnamed Windows job **before sending font bytes**:128MiB
committed process memory, one process, a requested1second user CPU limit, kill-on-close.
Windows checks CPU time periodically; CPU-stress verification did not establish a timely
OS CPU termination, so the parent wall deadline is the enforced timeout. The helper
verifies these fences before constructing untrusted caches. The parent checks the
job's cancellation/deadline and a1500ms wall cap, bounds replies to128KiB, kills/waits
on every refusal and joins both bounded pipe workers. Native error boxes are disabled.
No Lua, UI/render or audio thread waits on font library code.

At most8external shaping attempts may be admitted per job after options/input validation,
including native preflight or processing failures; the counter resets with the job.
Constants `max_external_shape_calls` (8), `max_external_shape_millis` (1500) and
`external_shape_memory_bytes` (134217728) are on `plum.text`. A process stop reports that
an invalid font, memory limit or native failure prevented a result; it does not guess
the cause. Timeout is a `time_limit` error, canceled work is `canceled`, native quota
and glyph/operation caps are `budget`. A helper refusal does not publish partial output.

The existing Harfrust expansion/operation/nesting bounds remain active. A vendored
additive status accessor lets the host reject exhausted native output explicitly;
shaping algorithms and dependency versions are unchanged. Both routes validate glyph
IDs, original UTF8 clusters, finite bounded positions and the full output cap before
returning Lua columns. Cancellation, malformed replies, native/VM quota or allocation
failure returns no partial run. Font close/job expiry/GC behavior is unchanged.
`max_shape_bytes`4096, `max_shape_scalars`256 and `max_shaped_glyphs`4096 remain on plum.text.
External shaping is currently Windows-only; bundled shaping retains its native route.
This does not add multiline/bidi layout, fallback, composed emoji, new outline formats
or full font compatibility; these remain separately documented requirements.

Text Art Studio now has three additional working shaped modes: proportional and
monospace `office é`, plus monochrome `😀🔥`. Lua explicitly positions/samples glyphs,
preserves their holes and maps cells to notes. All nine commands are exercised through
private Preview/discard/replay/Apply/native Undo/Redo in both key modes. Full font
fallback, installed-font shaping, multiline bidi layout, supported color technologies
and custom UI remain required development; do not infer universal Unicode coverage.

Verified bundled Noto emoji limitations in this build: 😀 and 🇺🇸 shape to one covered
visible glyph. 👍🏽 returns two glyphs with one missing; 👨‍👩‍👧‍👦 returns multiple covered
components rather than a composed family;1️⃣ also remains multiple glyphs. All retain
one original grapheme cluster. **missing_count0 does not establish composed emoji
support.** Applications must also inspect the returned glyph run/presentation and
report unsupported appearance. These uncomposed cases remain open acceptance work;
there is no silent invented replacement or claim of universal emoji support.

<a id="installed-font-catalogs-api02-development"></a>

## Installed font catalogs (API 0.2)

The separate **`fonts.list`** manifest capability permits installed family/face
metadata and loading a chosen installed face. Its grant starts empty. It grants no
arbitrary file access; external font files still require file.read_user_choice and
load_font through the approved dialog. Bundled fonts need neither permission.

`catalog=plum.text.installed_fonts(options?)` scans host system/per-user font directories
and the host's configured font override directories on the Lua worker. Options is a
plain table containing only `query` (UTF8 substring, at most128bytes, matched after
Unicode lowercase against family+style), `max_faces` (integer1–512, default512), and
`paged` (boolean, defaultfalse; opt-in continuation described below).
No plugin-provided directory/path is accepted. Symlinks are excluded from this
controlled scan. Existing native startup discovery keeps its prior behavior.

| Catalog method | Result |
| --- | --- |
| `info()` | `{count,truncated,scanned_files,scanned_entries,read_bytes,paged,page,has_more,scan_truncated}`. |
| `face(i)` | Independent plain `{index,page,face_index,family,style,weight,italic,bytes}`. |
| `load(i)` | Job-owned Font snapshot using the existing info/coverage/advance/glyf-outline APIs. |
| `close()` | True once; false after closure/expiry. Cleanup needs no capability. |
| `next_page()` | Advances an opt-in paged catalog; `true` for a new page, `false` when exhausted. Default catalogs refuse it. |

`i` is an integer1-based index in this catalog; face_index is the original0-based
font-collection face index. Family/style come from the existing name reader's preferred
names. Weight comes from OS/2 (valid1–1000; default400 if absent/invalid); italic comes
from OS/2/head (defaultfalse). A catalog is sorted by family/style with private source
and face-index tie breakers. Paths, file readers and native addresses never enter Lua.
All catalog reads and installed Font reads recheck the grant/current job.

```lua
local catalog=plum.text.installed_fonts{query="Arial",max_faces=64}
local scan=catalog:info()
if scan.count>0 then
  local choice=catalog:face(1)
  local font=catalog:load(1)
  plum.config.set("font_choice",{family=choice.family,style=choice.style})
  -- Use supported metrics/outlines or bounded isolated shape_run on this Font.
  catalog:close() -- The loaded immutable Font remains usable until job end/close.
  font:close()
else
  catalog:close() -- Report unavailable in this scan; don't silently invent a face.
end
```

Catalogs and their numeric indices are job-scoped, not preset identifiers. Persist
plain family/style preferences, rescan on a later job and explicitly report fallback
when the choice is unavailable in that scan. A truncated scan cannot establish that
a font is absent from the entire machine. Font Resource Inspector's third command
implements this flow and saves only plain preferences/report data.
Its fourth command, **Choose Installed Font…**, demonstrates a richer selector built
entirely from the public catalog and native UI services. It additionally requests
`ui.panels`; users grant it through normal plugin controls. The reusable
`font-picker.lua` helper is authored Lua, with64-face pages, explicit search/selection,
bounded scalar glyf outline preview and missing/unsupported messages. It saves no live
handles/paths. Choose writes one versioned plain `font_choice` with its metrics report;
Cancel preserves the old choice and legacy `last_report`. Save/Load choice preset is
an independent explicit action. Existing inspection commands keep their behavior.
The preview fits X/Y independently and does not claim shaped typography or color emoji;
see the example README for its31preview/one-final-load and native budget limits.

Limits:4catalog creations/job (closed catalogs still count); first16configured roots;
20000directory entries, depth4,1024candidate font files;512returned faces; names bounded
to512bytes,256faces per collection;256KiB per metadata read,16MiB cumulative metadata
I/O and16MiB name-decoding work per scan. Excessive names/tables/files/work produce a
bounded catalog with `truncated=true`; unsupported/malformed files may be skipped.
Scan workspace reserves24MiB native quota before traversal/parsing, then shrinks to
retained private paths/names. Cancellation checks occur through traversal, face parsing
and64KiB reads. Cancellation discards the entire unpublished catalog.

Installed loads share the32Font creations/job and64MiB input limit. They charge at
most256MiB of attempted snapshot bytes/job before allocation/read (failed attempts are
not refunded). Loading rechecks canonical host roots and source length/modification,
checks the actual normalized opened-handle path on Windows, reads in64KiB chunks,
rechecks source metadata after reading, then verifies the bounded face names against the captured row.
The controlled Windows reader denies concurrent write/delete sharing while its handle
is open; other existing file readers retain their behavior. Changed/unavailable
sources fail clearly; rescan instead of reopening an old unapproved path. Other host
file read/write capabilities are unchanged. No font hinting code is executed.

Catalog close/job end eagerly clears its private inventory. Each retained expired
catalog userdata keeps a1024byte native metadata reservation until Lua GC, so holding
old userdata cannot evade accounting. Loaded Fonts have their existing independent
lifetime/accounting. Constants max_font_catalogs4, max_installed_faces512 and
max_installed_font_bytes268435456 are on plum.text. Selected/installed/private fonts
support the isolated shaping route described above; private package assets use the
existing scoped asset stream. The rich searchable selector and remaining text/mask/
fallback/format and real Windows acceptance continue separately.

### Paged installed-font catalogs

`plum.text.installed_fonts{paged=true,max_faces=128,query="…"}` captures the same bounded
private candidate-file list once. `catalog:next_page()` continues its native file/face
cursor without re-reading earlier faces. Each page contains at most `max_faces` results
and is sorted by family/style/private source/collection index. Pages advance in private
source order; the concatenated pages are not globally sorted by family. `index` is
local to the current page; advancing replaces its rows. Copy plain display preferences
when needed and load a chosen face before advancing. Previously loaded Font snapshots
remain independent. Neither page numbers nor indices are persistent font identifiers.

`info().page` starts at1; `has_more` means candidate files/faces remain, including ones
that might not match the query. A page may be empty when a bounded scan makes progress
without matches. `scan_truncated` reports directory/file/collection/name limits;
`truncated` is true while more candidates remain or the scan is truncated. Once an
exhaustion probe returns false, the last accepted page stays readable and `has_more`
becomes false. `scanned_files` and `read_bytes` describe work for the accepted page
(a collection may be reopened on a later page); `scanned_entries` describes the initial
directory capture. Inspect `scan_truncated` before treating a missing choice as absent.

The original16roots/20,000entries/depth4/1024files/256faces-per-collection/name/read
bounds remain. Paging adds at most64page attempts per job across all paged catalogs
(`plum.text.max_font_pages`, including their initial pages and failed native attempts),
at most16MiB metadata I/O and16MiB decoding work per page, with the existing24MiB
native preflight for each page attempt. Retained cursor paths/page capacity/names stay
charged until replacement/close/job end. Cancellation and changed collection identity
during continuation close the cursor; no partial page is published. Native quota or
page-limit denial before advancing preserves the current page; native quota denial
before an attempt consumes no page allowance. Creating another catalog cannot reset
the allowance. Legacy non-paged scans keep their existing limits. A pathological single
face exceeding the work allowance fails with Budget rather than retrying forever.
All operations recheck `fonts.list`, job/deadline/cancellation; there is no new file,
dialog or executable authority. Inventories beyond these limits are not claimed complete.

```lua
local catalog=plum.text.installed_fonts{paged=true,max_faces=128}
local total=0
repeat
  local scan=catalog:info()
  for i=1,scan.count do
    local face=catalog:face(i)
    assert(face.page==scan.page)
    -- Present face.family/style in an authored native UI, or load(i) now.
  end
  total=total+scan.count
  if not scan.has_more then break end
until not catalog:next_page()
if catalog:info().scan_truncated then print("Font scan reached a documented limit.") end
catalog:close()
return total
```

<a id="immutable-native-view-snapshots-api-02-development"></a>

## Immutable native view snapshots (API 0.2)

`plum.ui.view_snapshot(surface)` returns an immutable, job-scoped view userdata or
`nil` if the requested view is hidden, not yet drawn, disabled or no longer matches
the current native document. Surface is exactly `"piano_roll"` or `"player"`.
Both need initially ungranted `ui.overlay`; Piano Roll also needs `project.read`,
and Player also needs `playback.read`. `ui.panels` is not needed for this read API.
Capture is enabled only while an enabled, ready plugin has the declared/granted
view authority. Installation grants none. This dependency exposes geometry;
it does **not** paint overlays or intercept pointer input.

```lua
local view=plum.ui.view_snapshot("piano_roll")
if view then
    local info=view:info()
    local uv=view:note_to_uv(info.origin_tick,60)
    local note=view:uv_to_note(uv.x,uv.y)
    assert(note.key==60)
    local screen=view:to_screen(uv.x,uv.y)
    local again=view:from_screen(screen.x,screen.y)
    assert(math.abs(again.x-uv.x)<1e-9)
end
```

`view:info()` returns a fresh plain table. Common fields: `schema=1`, `surface`,
exact integer `project`, `project_revision`, `view_revision`, logical-point
`bounds={x,y,width,height}`, and `pixels_per_point`. A view revision stays stable
while its native geometry/context/project facts match; resize, zoom, pan, DPI,
key mode, context, visibility or project changes produce a newer host revision.
A hidden/disabled view cannot reuse its previous revision when captured again.
Revision exhaustion disables capture instead of wrapping.

Piano Roll fields: context `track` ID, optional owned `content` ID, exact signed
`song_shift` **decimal string**, `locked`, `conductor`, `keys` (128/256), exact
`origin_tick`, `first_tick`, `last_tick`, `points_per_tick`, `row_height`, and
`scroll_rows`. IDs describe the drawn context and are not editing handles.
Ticks are the native local track/content ticks of the opened editor. The native
signed clip shift maps them to song time; it can be negative for trimmed contexts.
The host uses the same actual Geometry/TimeAxis/PitchAxis as drawing and input,
including the actual hosting Subtrack/content/lock/conductor context. It does not
create another pointer engine or scan project notes to capture these facts.

| Method | Result |
|---|---|
| `view:note_to_uv(local_tick,key)` | `{x=u,y=v}` at that tick and the center of the key row; Piano Roll only |
| `view:uv_to_note(u,v)` | `{local_tick,song_tick?,key?,pitch_position,inside}`; Piano Roll only |
| `view:local_to_song(local_tick)` | exact song tick, or nil outside0..2^63-1 |
| `view:song_to_local(song_tick)` | exact local tick, or nil outside0..2^63-1 |
| `view:to_screen(u,v)` | `{x,y}` in the same native logical-point coordinate space as bounds |
| `view:from_screen(x,y)` | normalized `{x=u,y=v}` for that immutable surface |

Normalized0..1 covers the note area (Piano Roll) or full Player display. Conversion
may return offscreen normalized points; native clipping belongs to display delivery.
`uv_to_note` follows native floor tick/row selection and reports a nil key beyond
the valid key rows. `pitch_position` is continuous, with integer values at row
centers. `inside` reports normalized0..1 bounds and does not grant editing eligibility.
Negative/overflow song-time results are nil; invalid keys/ticks fail clearly.
The methods use relative **integer** tick subtraction before floating-point screen
conversion, retaining large absolute ticks above2^53. Screen fractions still have
native viewport precision; keep musical/graph values as exact author data rather
than repeatedly reconstructing them from display pixels. All input coordinates
must be finite with absolute value at most1e12. No arbitrary OS pointer capture occurs.

Player fields: `keys`, native `mapping` (`time`/`tick`), `seconds_ahead`,
`rainfall_bounds`, and `keyboard_bounds` in the same logical-point space. Use
`from_screen` to normalize their corners. Player snapshots do **not** offer the
Piano Roll's note mapping: its keyboard/rainfall renderer has a separate policy.
The Player may still be preparing its native note snapshot. Its geometry read
neither starts audio nor replaces its compositor, glow, gradients or note colors.

Snapshots contain only bounded copied native facts, with no document/note ownership.
At most32snapshot resources are created per job, each reserving1024native bytes.
Retained userdata keeps its small metadata charge until collection. Every method
checks cancellation/deadline/lifecycle/current job; handles expire at run end and
cannot be reused in a later command or persisted as preset tokens. Returned tables
can be changed freely without changing native facts. Methods read the captured
view, not live mutable viewport state. Bounded overlay publication below consumes
these snapshots. Authorized metadata callbacks read the exact root-free view facts
attached to that native delivery, as described below; they do not acquire a document.

Source/host tests exercise exact large ticks, both key modes, signed clip shifts,
finite validation, permission separation, quotas/expiry and view revisions. Actual
headless native Piano Roll/App/Player frames cover drawn geometry, viewport changes,
visibility, revocation and unchanged1000note MIDI/history. These are distinct from
interactive Windows, final overlay rendering and dense-performance acceptance.

<a id="declarative-plugin-panels-and-canvas-api02-development"></a>

## Declarative plugin panels and canvas (API 0.2)

`plum.ui.open_panel(spec)` requires `ui.panels` and an explicitly user-invoked command
or non-preview custom-tool job. Loading/lifecycle/observer/provider jobs cannot open
panels. Existing ui.notify and API0.1 tool/curve forms keep their behavior. No Lua runs
on the rendering or audio thread. Keep the command running in next_event while the
panel is needed; job return/expiry/disable/reload closes its surfaces.

A spec is a plain `{title?,width?,height?,content=node}`. Title is at most128UTF8bytes;
default size640x480, width280–2048, height160–1536logical UI points. Native windows are
resizable and scroll vertically. Every node has a unique nonempty ASCII `id` of at
most64bytes (letters/digits/underscore/period/hyphen), `kind`, optional `label`, `tooltip`
and boolean `enabled` (defaulttrue). Labels/tooltips/text values are at most4096UTF8bytes.

| Node kind | Additional fields |
| --- | --- |
| column / row / group | plain dense `children`; row wraps, group can show label. |
| label / button | No additional fields; a button emits activate. |
| toggle | boolean `value`, defaultfalse. |
| color | dense RGBA integer-byte `value` (defaultwhite); native picker with alpha. UI appearance only. |
| tree | plain dense `items`, optional selected item-id string `value` (defaultempty), `height`64–1024(default200). |
| number / slider | finite `min`/`max` (default0/1, within +/-1e12), `value` within range, positive `step` (default0.01,1e-12–1e12). |
| text | UTF8 `value`, defaultempty; native single-line input. |
| choice |1–128dense text `choices` (each up to256bytes),1-based integer `value` default1. |
| tabs |1–16dense `children`, each an existing group with nonempty label up to256UTF8bytes;1-based integer `value` default1. |
| list |1–512dense text `choices` (each up to256bytes),1-based integer `value` default1; `height`64–1024(default160). |
| canvas | `height`64–1024(default320), boolean `interactive` defaulttrue, dense `primitives`. |

Canvas positions are normalized f64 coordinates:0,0top-left;1,1bottom-right. The author
maps these to any mathematical domain and keeps original values separately. Drawing
coordinates within +/-16allow clipped off-screen geometry; use geometry.clip_segment
for greater excursions. Primitive order is layer order, clipped to the canvas and UI
viewport. Optional color is a dense array of four integer RGBA bytes (default180,180,
220,255); this is UI color, not per-note color storage. Width is logical points0.25–16.

| Primitive kind | Fields in addition to kind/color |
| --- | --- |
| line | x1,y1,x2,y2,width? |
| polyline | dense equal x/y columns (at least2points),closed? boolean,width? |
| rect | x1,y1,x2,y2 ordered bounds,fill? boolean,width? |
| circle | x,y,radius? in logical points0.5–32(default4),fill?,width? |
| text | x,y,text up to256UTF8bytes; fixed native body font size. |
| gradient | x1,y1,x2,y2 positive rectangle; explicit RGBA top_left,top_right,bottom_left,bottom_right; native4vertex/2triangle color interpolation,4point cost. |
| image | `image` UI image userdata; ordered positive x1,y1,x2,y2 rectangle; optional u1,v1,u2,v2 UV crop within0–1 (defaults0,0,1,1); optional RGBA tint (defaultwhite),4point cost. |

`panel:update(spec)` validates/replaces the snapshot and returns its new revision.
Control values in the new spec are authoritative; carry the state you intend to keep.
`panel:info()` returns revision,closed,queued,coalesced. `panel:next_event(timeout_ms?)`
waits on the worker with cancellation checks (default until event/closure; optional
integer0–60000). It returns an independent event table, or nil on timeout/programmatic
close. `panel:close()` closes without canceling, permitting an explicit Apply path;
true once, false thereafter. `panel:cancel()` cancels the owning job. Close is always
available for cleanup. Stale job handles cannot update/read a later job's panel.

Events have kind,id,revision,sequence; `change` has typed boolean/number/text/1-based
choice/list/tab value. Kinds: opened,activate,change,resize,pointer_down,pointer_move,pointer_up,
pointer_cancel,scroll. Pointer/resize events carry a `pointer` table with x,y,width,
height,buttons,shift,ctrl,alt,command,scroll_x,scroll_y,scroll_unit,time. Button bits are
1primary/2secondary/4middle. Coordinates can exceed0–1 during a captured gesture that
started inside the canvas; input elsewhere is not delivered. Time is captured native
frame time in seconds, not a per-device timestamp/pressure guarantee. Scroll retains
native units `points`/`lines`/`pages`. Input processes at most256ordered native events
per canvas/frame before overload handling; it never calls Lua per raw OS event.

Use each event's **render revision** to select the author's mathematical mapping that
was shown; it can differ from the latest update revision. Retain a bounded mapping
history or discard stale input safely. A resize changes surface dimensions, not the
author's original numerical values. Consecutive compatible changes/moves/resizes
coalesce; scroll deltas add only with matching target/revision/buttons/units. Down/up
boundaries remain ordered. Coordinate accuracy at narrowing/DPI and rapid ordered
press/move/release is tested through the real headless native egui path.

```lua
local panel=plum.ui.open_panel{title="Author controls",content={
  id="root",kind="column",children={
    {id="amount",kind="slider",label="Amount",min=-1,max=1,value=0},
    {id="finish",kind="button",label="Finish"},
  }}}
while true do
  local event=panel:next_event()
  if not event then break end
  if event.kind=="change" then print(event.id,event.value) end
  if event.kind=="activate" and event.id=="finish" then panel:close();break end
end
```

Limits:4panel creations/job;8active frontend panels;256nodes/depth8;2048primitives and
8192point-cost units per snapshot (line2,rect4,circle64,text4per scalar,polyline its point
count);64KiB total snapshot text;32768total choice/list options across all nodes.2MiB transient/native snapshot reservation plus512KiB
shared queue/state reservation; closing clears geometry/queues, retaining2048bytes per
old userdata until GC.4096updates/1MiB cumulative snapshot text/job;64queued events and
65536native event submissions/panel/job. Overload fails the job. Renderer snapshot
reads use try-lock and keep the last snapshot during worker publication; queue locks
cover only bounded native push/pop and are released before Lua, parsing or waits.

The unforgeable native Cancel plugin button and window X cancel the whole job.
Permission/project-revision/deadline/disable/reload checks continue each frontend frame.
End-run freezes every surface and honors fatal errors even if Lua caught them with
pcall; a final host budget check also prevents caught cancellation from publishing
staged commits. Musical changes still require their existing capabilities/transactions,
revision checks and confirmation/Undo rules. UI colors/overlays create no project data.

Trees accept at most512items across the entire snapshot, including hidden descendants,
and at most8levels within each tree. Each item is a plain descriptor with a unique
local ASCII `id` (same1–64byte alphabet as node IDs), nonempty `label` (≤256UTF8bytes),
optional `tooltip` (≤1024bytes), boolean `enabled` (defaulttrue), boolean `expanded`
(defaultfalse), and optional plain dense `children`. Empty arrays are supported. Item
IDs are local to their tree; event.id names the control. All item text shares the64KiB
snapshot text budget. `value` is an existing item ID or empty for no selection.

A native tree label click emits change with the selected ID string. An arrow click emits
expand with value={id="item-id",expanded=true_or_false}. Expand boundaries remain
ordered; label/color changes may coalesce as other change events do. A color change
carries a new dense four-byte RGBA table. All are independent worker-owned Lua tables.
The author retains tree expansion/selection and color in its model before updating the
spec; a newly published snapshot's values/expanded flags are authoritative. Disabled
branches and descendants reject pointer interactions. Tree/list rows are virtualized
and limited to one visible line, with complete labels available on hover (a tree tooltip
overrides its label). This preserves fixed row geometry even for multiline source text.
The constant plum.ui.max_tree_items=512 applies globally per snapshot.

Four-corner gradients use two native triangles and interpolate premultiplied UI vertex
colors; they are not a general nonlinear/bilinear mathematical evaluator. The author can
compose primitives or Image resources for other authored mappings. Existing canvas
clipping/layer order/point/primitive budgets apply, with no texture upload for a gradient.

Tabs render only their selected group. A disabled group has a disabled tab header.
Hidden canvases release pointer capture and emit pointer_cancel before accepting any
further gesture; inactive content receives no pointer input. Lists reuse native
virtualized scrolling and render viewport rows rather than one widget for every item.
The author retains selected values and graph coordinates when publishing new specs;
all hidden nodes still count toward snapshot limits. Constants: plum.ui.max_tabs=16,
max_list_options=512,max_ui_options=32768. The existing choice limit remains128.

Install **UI Layout Laboratory** from the example installer for a complete ui.panels-only
command with draggable author coordinates, tabs, disabled tab,512item list, a tree,
RGBA/gradient controls and version1 plain control presets. Save/reset/load demonstrates
author state retention without musical edits. It reads and modifies no project data.

This is the initial panel/canvas framework. Font-selector widgets, editor-surface adapters and full interactive/performance/release
acceptance remain required work. Headless egui tests are not actual Windows GUI tests.

## Bounded canvas image resources (API0.2)

`plum.ui.image(source_image[, {max_side=512,filter="linear"}])` creates an opaque,
immutable UI image from an existing Image resource. It requires ui.panels OR ui.overlay and an
explicitly invoked command/non-preview tool job. Procedural Image data needs no file
authority; selected files still use file.read_user_choice and the existing decoder.
There is no path-based image load or GPU handle in Lua. The source Image must belong
to the current job and be open at conversion; it can be closed afterward.

`max_side` is a strict integer1–1024. Smaller images are not upscaled. Larger images
fit within that side with integer dimensions, preserving aspect to rounding/minimum1.
Downsampling reuses the native box algorithm. `filter` selects native texture sampling
(`linear` default or `nearest`), separately from the box downsampling. Pixel conversion
and cancellation checkpoints run on the worker. Rendering reads prepared immutable data.

UI image methods: `info()` returns width,height,source_width,source_height,bytes (RGBA
payload),filter,origin="top_left". `close()` returns true once, false thereafter and
releases the pixels/texture; an existing primitive becomes invisible. Close is always
available for cleanup. An ordinary UI image expires at job end, even when its Lua
userdata is retained; it cannot be published in another job. See the explicit
project-scoped promotion below. Native references contain no transferable file grant.

```lua
local source=plum.images.from_rgba(2,1,string.char(255,0,0,255,0,255,0,128))
local picture=plum.ui.image(source,{max_side=512,filter="nearest"})
source:close()
local p=plum.ui.open_panel{title="Image",content={id="preview",kind="canvas",primitives={
  {kind="image",image=picture,x1=0,y1=0,x2=1,y2=1,color={255,255,255,200}},
  {kind="line",x1=0,y1=0,x2=1,y2=1,color={255,200,0,255}},
}}}
-- Process typed events on the worker; close from an author control when finished.
while p:next_event() do end
picture:close()
```

Images follow canvas layer order and clipping; their rectangle is authored independently
of source aspect. UV coordinates specify a positive crop; use existing Image transforms
before conversion for rotation/mirroring. Tint/alpha are UI appearance and create no
per-note colors. Repeated snapshots and separate panels share one native texture per
UI image. Initially off-screen/hidden geometry does not upload; a prepared texture is
cached until resource/job closure. No pixel processing or Lua runs on the renderer.

Limits:16created resources/job,4,194,304cumulative attempted output pixels/job, side1024.
Closing does not restore these creation/work quotas. Native preflight covers20bytes per
output pixel plus resize scratch during conversion, then conservatively reserves16bytes
per pixel for CPU/texture/in-flight storage and2048metadata bytes/resource. This is an
allowance, not a measured GPU-driver heap report. Frontend references retain accounting
until released; retained closed userdata keeps metadata until GC. Uploads are limited
to1,048,576new pixels per native paint pass across all panels; deferred images appear in
a later repaint. A smaller backend texture limit fails the job with an actionable Budget
error instead of attempting an invalid upload. Normal plugin/global native quotas also
apply. Public constants are max_ui_images,max_ui_image_side,max_ui_image_pixels and
max_ui_upload_pixels in plum.ui. Errors/cancel/end-run discard unpublished resources;
UI changes do not create musical transactions or Undo steps.

Install **UI Image Laboratory** for a complete authored checker/alpha preview, opacity,
UV crop, pointer marker and native file-selection example. It requests only ui.panels
and file.read_user_choice and has no project-editing authority. Live UI images cannot
be serialized; presets should store plain author parameters and request files again.

<a id="project-scoped-retained-ui-images-api02-development"></a>

## Project-scoped retained UI images (API 0.2)

`plum.ui.retain_image(ui_image)` requires ui.overlay, an open project and an explicitly
invoked command/non-preview tool job. It returns an opaque UI image sharing the source's
already converted immutable pixels and any existing native texture. It does not decode
again, retain the source Image or extend a selected-file grant. The source UI image
still expires at the end of its job. Opening panels still requires ui.panels.

```lua
local source=plum.images.from_rgba(2,1,string.char(255,0,0,255,0,255,0,128))
local picture=plum.ui.image(source,{max_side=2,filter="nearest"})
retained_guide=plum.ui.retain_image(picture)
source:close()
assert(retained_guide:info().lifetime=="project")
```

Later successful jobs in the same loaded plugin Host and project may reference
retained_guide. Its info() has the ordinary fields plus lifetime="project". close()
releases its pixels/texture immediately; already submitted native consumers retain
their own accounting until released. Closing the source does not close the retained
resource. Repeated promotion of the same pixels/filter shares the retained resource,
so closing any promoted alias closes all aliases. Tokens cannot be serialized or
transferred to another plugin, Host, project or preset; save plain author settings.

Limits are16live retained images and4,194,304live retained pixels per Host, exposed as
plum.ui.max_retained_ui_images and plum.ui.max_retained_ui_pixels. Promotion reserves
2048native metadata bytes before creating a resource; existing pixel accounting is
shared. Each job permits16promotion attempts, including aliases and refused attempts;
ordinary conversion quotas still apply independently. Closed userdata retains its
metadata charge until GC. Normal plugin/global native budgets remain authoritative.

A failed or canceled job discards its newly promoted resources while preserving
previous successful resources. Project switch/close and plugin disable/reload/removal
invalidate retained assets. Project close advances a native generation immediately
and queues bounded coalesced worker cleanup without a Lua callback; queued older jobs
cannot revive assets. These resources provide lifetime ownership for UI composition;
this section does not introduce an editor-overlay publication function. Existing UI images keep their job-scoped expiry; API0.1 behavior remains unchanged.

<a id="staged-editor-overlay-frames-api02-development"></a>

## Staged editor overlay frames (API 0.2)

`plum.ui.publish_overlay(view, {id="guide", primitives={...}})` stages an immutable
native display list for the opaque current-job view from ui.view_snapshot. It requires
ui.overlay and project.read for Piano Roll, or playback.read for Player. The spec is
strict and accepts only id/primitives. IDs contain1–64ASCII letters, digits, underscore,
hyphen or dot. The exact same canvas primitive schema, coordinates, colors and limits
apply:2048primitives,8192weighted points and64KiB total text per frame. Text primitives
have at most256UTF-8 bytes each. Native accounting reserves2MiB per primitive list plus
2048bytes per frame before allocation; normal plugin/global budgets remain active.

```lua
local view=plum.ui.view_snapshot("player")
if view then
  plum.ui.publish_overlay(view,{id="guide",primitives={
    {kind="line",x1=.25,y1=.5,x2=.75,y2=.5,width=3,color={31,117,233,127}},
    {kind="text",x=.25,y=.5,text="Author-defined guide",color={255,220,80,255}}
  }})
end
```

`plum.ui.clear_overlay(view,id)` stages removal of that plugin's ID on that view's
surface. It accepts the same opaque view authority. It cannot clear another plugin's
frame. Both functions return no value: they stage submission, not a display or GPU
acknowledgement. Changes leave the worker only after a successful job. A failed or
canceled job discards its entire staged batch; a full/closed native mailbox fails the
job with budget and publishes nothing. At most4publication/clear attempts per job
are admitted before parsing, including invalid attempts. Repeated successful changes
to the same surface/ID coalesce to the final change in that job. Each plugin owns at
most4IDs across both surfaces and submits at most20successful batches per second.
The one-batch native mailbox does not grow when the UI is busy. There is no retry.

The native App admits at most8owned frames globally; exceeding that cap refuses the
whole incoming batch. The host also discards stale submissions without displaying
them. Publication is authorized UI work on the worker, including appropriately scoped
read-only work, without granting musical mutation or unrestricted native callbacks.
Explicit commands and bounded authorized observation callbacks can submit frames.
Automatic observer dispatch waits while a prior native batch remains unconsumed; native
polling acknowledges or discards it even on hidden surfaces. This keeps one-batch
backpressure without pausing an observer through a command/publication race. Explicit
commands still receive the documented full-mailbox refusal. The constants
max_overlay_frames/max_visible_overlays report the caps.

Each frame must still match the exact native view and project revision when displayed.
Zoom, pan, resize/DPI, key mode, Track/Clip/context, native document revision or hidden
surface changes discard it. Recreate a current-job view and publish again; a frame is
never silently reinterpreted in another viewport. Project close advances native
lifecycle generation immediately; queued older jobs and frames cannot revive it.
Plugin disable/reload/removal, authority revocation and Host destruction also remove
owned frames. Frames retain copied metadata/primitives, not a document/note root or a
job lease. API0.1 curve, drawing, panels and note rendering remain in place.

Image primitives must use ui.retain_image pixels: ordinary job-scoped UI images are
rejected. No selected-file consent, decoder, raw GPU handle or source Image crosses
into rendering. Same retained pixels and texture caches are shared. Native uploads
are limited to1,048,576new pixels per whole App frame across overlays and panels,
with off-screen images culled and excess uploads deferred. Unsupported renderer side
limits cause actionable budget errors on subsequent resource access. Closing an image
makes its primitive invisible and releases resources as native consumers drop them.

Piano Roll frames are clipped to its actual note area, after native note fills and
before Preview, selection handles and playhead. Player frames are informational,
after its native rainfall/keyboard and before native hints/readouts; they do not use
Piano Roll coordinate mapping. Overlays install no hit regions and intercept no input.
No Lua/pixel conversion runs during paint/audio. Disabling a displayed overlay through
Lua Plugins → Editor overlays preserves the plugin's other features. Choices last for
the current session (at most64disabled IDs retained, the lexicographically first stored entry evicted at
capacity); they are not serialized as musical content or written to plugin presets.

Install Editor Overlay Laboratory for authored checker/selected-image luminance-mask
guides and captured Player transport readouts. Its nine commands include explicit
opt-in automatic guide/Player refresh using the observation API below. It uses retained
pixels and exact delivered views; clear commands also stop the corresponding auto mode. It creates no
notes and has no note-editing authority. Source/host/native App evidence is distinct
from actual Windows interaction, dense-project performance and release delivery gates.

<a id="surface-notifications-and-exact-reactive-view-delivery-api02-development"></a>

## Surface notifications and exact reactive view delivery (API 0.2)

Subscribe to surface_changed while loading, using the existing plum.plugin.observe
registration. ui.overlay and at least one surface read permission are required:
project.read authorizes Piano Roll; playback.read authorizes Player. The native manager
filters events and copied geometry per surface. A Player-only plugin receives no Roll
surface events, coordinates or Track/Clip identities. Project-store observation is
not activated by a surface-only subscription. Existing view_changed and project.observe
requirements remain unchanged. Mixed subscriptions require every applicable permission.

```lua
plum.plugin.observe{
  id="surface-guide",events={"surface_changed"},debounce_ms=75,
  run=function(event)
    if not event.data.visible then return end
    local view=assert(plum.ui.view_snapshot(event.data.surface))
    assert(view:info().view_revision==event.data.view_revision)
    plum.ui.publish_overlay(view,{id="reactive-guide",primitives={
      {kind="line",x1=.1,y1=.2,x2=.9,y2=.2,color={255,210,80,160}}
    }})
  end
}
```

The usual schema1 metadata envelope has read_only=true and transaction_allowed=false.
data is `{surface,visible,view_revision?}`; hidden surfaces have visible=false and no
view_revision, and view_snapshot returns nil for them. Returned Lua tables do not
modify native source facts. Only native owners create deliveries. Callbacks run on
plugin workers with the existing250ms automatic deadline, cancellation and instruction
limits. They receive no operating-system pointer stream and execute no code on paint
or audio threads. UI publication does not grant musical edits or transaction responses.

Native notification publication is sampled at most20Hz; small view/lifecycle guards
remain current each App frame. Changes to each surface coalesce independently within
an observer's existing64entry bounded queue; Roll and Player transitions cannot overwrite
one another. The existing32subscription/debounce/overflow limits and at least125ms
between automatic worker jobs apply. User work and native mailbox consumption take
priority; notifications are latest-state signals rather than a complete event log.
The disabled path performs no note/selection scan or document snapshot acquisition.

At dispatch, the native owner attaches at most2authorized immutable view facts and,
when playback.read applies, one finite captured transport snapshot. These are copied
metadata with no document, note, synth, file or GPU ownership. In the callback,
ui.view_snapshot reads this fixed delivery and its handles expire at job end.
playback.snapshot reads the exact transport event for transport_changed; other authorized
overlay metadata callbacks read their captured dispatch snapshot. A callback does not
get a live project root merely because its UI lease carries the native project stamp.

View/project/context changes invalidate running geometry deliveries before another
frame can use old coordinates. Stale cancellation does not pause later subscriptions.
Pending surface events are checked against current native view revisions at dispatch;
Host/permission/project-close lifecycle fences remain authoritative. Old frames are
still rejected by the native display owner. Geometry updates cancel only obsolete
worker work and preserve already accepted independent native MIDI output.

Editor Overlay Laboratory keeps automatic modes off until its Enable Automatic Guide
Refresh or Enable Automatic Player Readout command. The author then composes retained
image pixels, surface_changed and transport_changed callbacks. Clear disables the
corresponding author mode; project close resets both modes. The example logs skipped
visual refreshes and avoids retry loops. The host supplies bounded infrastructure;
the Lua author writes placement, masks, formatting and refresh behavior.

## Author-written custom graph example

Install `examples-lua/custom-graph-laboratory`. Its three commands demonstrate cubic
Bézier control points, signed polar rose mathematics and an editable linear/step MIDI
envelope without the native curve parameter. All mathematics, hit tests, model/view
mappings, gestures, preset schema, deterministic randomness and note interpretation
are authored Lua. Use these as complete examples of open_panel/update/next_event plus
geometry clipping, actual selected-note queries and transactions/streamed note writing.

Keep original f64 domain coordinates separate from normalized rendering coordinates;
associate incoming events with the view for their render revision. This sample uses an
eight-revision bounded history, clips 256 segments and limits MIDI generation to512.
Changing graph/view/parameters invalidates the captured Preview. Apply uses that model
and seed, closes the panel and publishes one native Undo step. Explicit Save preset is
an independent atomic file action; canceling later MIDI work does not undo that save.

Windows checks cover the authored panels, pointer editing, wheel zoom, numeric pan,
preset restoration, corner growth and shrink, and envelope Preview, Apply and Undo.
The native curve editor remains available for API 0.1 tools. See the release evidence
for tested surfaces, project sizes and UI scales.

<a id="custom-piano-roll-strokes-api02-development"></a>

## Custom Piano Roll strokes (API 0.2)

A tool registered through the existing `plum.plugin.tool` can opt into native Piano
Roll freehand capture with `gesture="freehand"`. Omit gesture to retain the API0.1
parameter/curve tool behavior. Set `needs_selection=false` for a generator that does
not require selected notes. The manifest must request `tools.draw` and the user must
grant it; this permission only receives strokes while this particular tool is selected.
Project reads and musical transactions still require their separate existing grants.

The existing native Piano Roll capture converts positions using its current X/Y zoom,
scroll, key mode and edited Track/Subtrack/Clip target. Partial samples remain native
and appear only as a temporary stroke path. Only a completed gesture is delivered to
the Lua worker through typed Rust data. No Lua runs in a raw pointer, rendering or audio
callback. The custom adapter retains up to256ordered native egui input events per frame, sampling
movement after2logical UI points and retaining the release endpoint, with at most65,536
samples. An excessive native batch cancels the active stroke. It stops adding samples at that limit; info
reports at_sample_limit so an author can refuse or ask for a shorter stroke. This is
not a per-device pressure or raw OS event recording interface.

`ctx.stroke` is an immutable job-scoped userdata for a freehand tool run. It is absent
on legacy tools. It cannot be constructed from a Lua descriptor. Methods:

| Call | Result |
| --- | --- |
| stroke:info() | count,track_id,optional content_id,space="target_local",song_shift (signed exact decimal text),keys,ppq,snap,min_tick,max_tick,duration; shift/ctrl/alt/command flags with modifiers_at="completion"; sampling="native_frame",ordered_events=true,max_events_per_frame=256,at_sample_limit,pressure_supported=false,tilt_supported=false. |
| stroke:point(index) | Independent plain sample table; index is an integer1–count. |
| stroke:batch(first,count) | Dense independent sample tables; integer count1–1024, entirely within the captured stroke. Process in batches to keep Lua memory bounded. |
| stroke:close() | Releases this worker's sample reference; true once, false thereafter. Cleanup is allowed after expiry. |

Each sample has exact integer `tick` on the edited target's local axis, fractional
`pitch` in note rows, captured relative `time` in seconds (native handling time; events in one frame do not have individual device timestamps), optional exact nonnegative
`song_tick` and always-exact signed `song_tick_text`. Song tick = local tick + captured
song_shift; values outside0–2^63-1 have no numeric song_tick rather than wrapping or
clamping. Keep local ticks for edits to ctx.track, including a clip's content.

Sample `x` is normalized over the stroke's minimum/maximum local ticks (0for zero span),
`y=pitch/keys`; these are independent of the screen viewport. `distance` is the previous
sample's segment length in beats horizontally and rows vertically. `direction_x/y`
are its unit vector, or zero for no movement. `speed` is that distance/time, with positive
time differences floored to1ms (zero for equal timestamps). `start`/`finish` mark ordered
first/last samples. Integer differences are computed before floating conversion.
Pressure/tilt and per-sample modifier history are not supplied by this initial adapter.

The native buffer uses existing Stroke/StrokePoint structures. Input validation rejects
nonfinite values, unordered/negative timestamps, times beyond24hours, pitches beyond
+/-4096 and local ticks beyond2^63-1. A worker preflights24bytes/sample plus512bytes and a
512byte handle reservation. Job end/cancel/unload eagerly drops samples; retained old
userdata keeps only its512byte accounting reservation until GC and cannot read a later
job. Batch loops check cancellation every64samples. At most one stroke handle/job.

Preview uses the existing custom-tool worker and native temporary note preview. Apply
commits exactly the current kept result as one native Undo step; it clears the completed
stroke and keeps the tool open for the next gesture. Redo uses native history. A new
stroke replaces an unapplied stroke. Escape cancels an active stroke, Clear stroke drops
its pending result, and closing the tool cancels its worker. Editor target/clip-origin/
key-mode changes invalidate captured input; locked contexts, disabled/unloaded plugins
or revoked drawing permission close capture. Old generations cannot publish results.
A changed Preview must be recomputed before drawing Apply; legacy tools retain their
existing automatic rerun behavior. Existing transaction validation/locks/confirmation
limits still apply, and failures never publish half a stroke.

```lua
function register(plugin)
  plugin.tool{id="my-brush",name="My authored brush",gesture="freehand",
    needs_selection=false,run=function(ctx)
      local count=ctx.stroke:info().count
      local writer=ctx.tx:note_writer(ctx.track)
      for first=1,count,1024 do
        local notes={start={},gate={},key={},velocity={}}
        for i,p in ipairs(ctx.stroke:batch(first,math.min(1024,count-first+1)))do
          notes.start[i]=p.tick;notes.gate[i]=60
          notes.key[i]=math.floor(p.pitch);notes.velocity[i]=90
        end
        writer:append(notes)
      end
      writer:finish() -- the host commits ctx.tx after run returns successfully
    end}
end
```

This initial adapter provides completed freehand gestures and native stroke/note
previews. General custom overlays/handles, richer bounded live stroke event streams,
tablet pressure/tilt when available, additional gesture adapters and dense/RSS/audio/
actual Windows drawing acceptance remain required work. Native Draw/Paint and API0.1
curve tools coexist; this does not expose arbitrary operating-system input.

### Custom Drawing Studio and native capture acceptance

Custom Drawing Studio is included in Plugins → Lua Plugins → Examples, alongside
Custom Graph Laboratory and both original API0.1 examples. Installation grants nothing
automatically. Its five authored tools demonstrate general geometry, bounded stroke
reads, streamed notes, deterministic scatter, onset-region deletion and native presets.

The custom freehand adapter retains ordered native egui events in the existing stroke
buffer (up to256events/frame); a complete down/move/up batch retains its first/interior/
release samples even within one frame. Oversized batches cancel active capture. Native
Draw/Paint retain their original capture mode. Custom tool parameter scrolling reserves
space for actions, and each preview result has a process-wide render identity so opening
a different tool cannot reuse the prior tool's cached note mesh.

Source/native headless tests verify all five algorithms in128/256keys, captured-input
replay, Preview/Apply/Undo/Redo, seed variation, preset parameter roundtrip, clip ownership,
Escape/revocation, event overflow and actual mesh invalidation. Actual Windows checks
separately verified freehand210added/Undo/Redo, rectangle216added/Undo, spray5added/Undo,
eraser19affected in Preview followed by Cancel, velocity101preset save/reset/load, and
the83note three-cycle wave preview switching to a different rectangle without a project
edit. Eraser/Wave Apply in Windows, dense/RSS/audio, stylus/live overlay/resource composition,
subtrack and all remaining acceptance gates are not inferred from these partial checks.


<a id="registered-format-adapters-api-02-development-backend"></a>

## Registered format adapters (API 0.2)

`plum.plugin.format{ id, name, extensions, import?, export? }` registers author-written
codecs while a plugin loads. Enabled adapters appear under Plugins → the plugin name,
with explicit Import/Export actions for the operations registered. Install the embedded
Note Exchange example to try an authored streaming format. Native File → Open/Save and
default MIDI/LUP handlers are preserved; plugins never replace them automatically.

Declarations are plain tables with only those fields. `id` is1–64ASCII lowercase
letters/digits/period/underscore/hyphen. `name` is1–64printable Unicode scalars, at most
256UTF8bytes, without controls. `extensions` is a plain dense array of1–16unique
case-insensitive ASCII letter/digit extensions (1–16bytes each, without dots/paths).
`lup`, `mid`, `midi`, `rmi` are reserved. At least one of `import`/`export` is a function.
At most `plum.plugin.max_formats`32declarations per plugin; duplicate IDs, unknown
fields, malformed UTF8 and registrations after load fail. Declarations and callbacks
are captured; later edits to the author's input table do not alter the registration.

Native dispatch is typed, explicit and single-use. The host selects one file with the
registered filter, checks the chosen extension and supplies `ctx.file`, `ctx.project`,
`ctx.keys` (128/256), `ctx.format` and `ctx.mode` (`"import"`/`"export"`). Paths never
enter Lua. Canceling selection runs no codec. Streams expire at job end and retain
existing read/seek/write/chunk/total-byte/native cancellation and filesystem protections.
The codec can call the documented stream methods and pure image/font/geometry services;
it cannot request another file dialog. Private package assets remain separately scoped.

Import needs `project.read`, `notes.edit`, `file.read_user_choice`; additional native
Track/Conductor/metadata edits still need their existing grants. `ctx.tx` is the single
host-owned transaction. Return normally to commit its private staging automatically,
or commit it explicitly once. Extra transactions are refused. Failed/canceled codecs
produce no commits; successful imports return private commits for native review, never
ordinary command auto-apply. Apply must use that result/revision and native Undo history.
The native Review Import window keeps the result private. Apply uses exactly those
commits; Cancel/window-close/Escape discards them. Project/revision, current grants and
the exact loaded-plugin lifecycle are checked again. A busy project retains its preview
for a later Apply; reloading/disabling/closing the project invalidates it. Musical edits
are locked while reviewing; native viewport navigation remains available. The current
Track/Clip's staged notes use the native renderer and Track/Subchannel color model;
resulting Track counts are listed, including newly created Tracks. A native summary
shows before/after stored Track and owned Clip note/event counts, explicit Clip
placements, Conductor entries, groups and plugin attribute records. Change indicators
also detect equal-count event, tempo, meter, marker, Master Volume, group and attribute
edits. Track note changes use native content revisions, without scanning notes. Linked
placements share content and do not multiply the stored-note totals; unchanged primary
placements are implicit. At most 32 changed-Track samples use labels of at most 80
characters plus a truncation mark. The worker prepares the report under native memory,
time and cancellation budgets before review; comparison failure discards the private
import. Ordinary API 0.1 transactions do not prepare a report.

**Inspect staged result** opens a separate read-only native browser; clicking a result
Track opens that Track there. Its Notes tab reuses the Piano Roll renderer and native
128/256-key and Track/Subchannel colors, including newly created Tracks and owned Clip
contents. Wheel navigation, Ctrl-wheel X zoom and Ctrl-Shift-wheel Y zoom use native
viewport controls. The Content tick field parses exact integer ticks without floating
point conversion. The review grid shows ticks; the Conductor tab lists the actual
tempo, meter, marker/chapter and Master Volume maps separately. No editing, selection,
audition or plugin pointer/overlay delivery occurs on this private surface.

Events shows virtualized exact local ticks, decoded CC/Pitch Bend values and stored
bytes; variable meta/SysEx payload excerpts show at most 32 bytes. Track settings shows
native properties and Subchannels. Clips lists explicit placements and opens their
shared content, with host/content identity, song start, offset, window length and mute
state. Notes/events show the complete shared content in local ticks, including portions
outside a placement's playback window; the ruler is shifted to song time. Unchanged
primary placements are available through their Track. Groups shows native hierarchy,
locks and links. Attributes shows namespace/target/version and paged typed values,
at most 8192 characters per page, preserving exact integers and finite numeric values.
Names and native property excerpts are bounded. Tables create only visible rows; the
browser holds view state and borrows the existing private result, without building
project-sized Lua tables or a second note vector. Closing this browser keeps the
import review available; Apply/Cancel and project/plugin invalidation retain the
same native transaction rules. Actual interactive Windows acceptance remains separate.
Deletions of a million notes or more retain the existing
refusal/confirmation limits; this interface does not bypass them.

Export needs `project.read` and `file.write_user_choice`. It has no `ctx.tx`, project
writes are refused, and `ctx.file:commit()` cannot publish early. The host publishes
only after successful whole-job validation, using existing staged flush/sync/identity/
atomic-rename protections. Failure/cancellation/VM/native/output-limit errors discard
staging; a changed destination is preserved and reported. Cancel after an already
completed atomic publication cannot undo that filesystem action. File publication and
musical Undo are distinct. Panels, overlays, playback, MIDI input and drawing
permissions are unavailable in codecs; installed font listing retains its grant when
present. Declared, already granted `services.call` is retained for bounded pure-service
composition, as described below.
Configuration/preset writes are refused in this scope, and project-setting migrations
cannot escape through a format result. Normal commands keep their original authority.

### Composing codecs with declared pure services

Import and export adapters may use the existing `plum.services.call(namespace,data,
options)` when the codec manifest declares `services.call`, the user grants it, and
`service_dependencies` declares the exact provider namespace/version. There is no
implicit grant or dependency installation. The provider must be enabled, ready and
granted `services.provide`, just as for ordinary service calls.

Only bounded plain data crosses workers. Read/decode the chosen stream in the codec,
then send a supported chunk, numeric columns or explicit mathematical parameters to
the provider. Do not send `ctx`, `ctx.file`, `ctx.tx`, project handles, fonts or images:
userdata, functions and metatables are rejected on both sides. Existing limits remain
64 KiB per structured payload, 16 KiB per string, 2,048 values, depth 8, 64 calls per
root job and 10–250 ms per call, with inherited deadlines/cycle checks. Large codecs
should keep their streaming parser local or design a pipeline within these budgets.

Provider effective grants are the intersection of its own grants and the codec's
**narrowed format authority**. Import editing permission does not let a pure provider
edit: it receives no caller project World, transaction or file stream. Service scope
still refuses transactions, settings/preset writes, registrations, UI/file prompts
and device access. The codec validates the reply and stages notes through its own
native `ctx.tx`, or writes encoded bytes into its own protected export stream.

Service errors, unsupported replies, timeouts, caller/provider cancellation, grant
revocation, reload and project revision expiry propagate through the existing broker.
An uncaught error discards accepted private import staging and any staged export
prefix. A successful import remains a native review result with one musical Undo;
export publishes only after the complete successful job. Calls do not weaken file
selection, atomic publication, native locks or source/lifecycle checks. If author code
catches an error, it must explicitly decide whether its remaining local result is
valid. Neither a seed nor IPC makes arbitrary author code deterministic; Preview/Apply
uses the captured staged result rather than rerunning a provider during Apply.

This explicit example defines a simple four-column text format for the first Track.
The importer deliberately caps input at64KiB; a large-format author should implement
incremental parsing with `ctx.file:read()` and bounded writer batches. Blank body lines
are ignored. Export streams selected packed columns, without materializing the project.
It preserves exact integer ticks and uses native Track/Subchannel note colors.

```lua
function register(p)
    p.format{id="notes",name="Author Notes",extensions={"pnotes"},
        import=function(ctx)
            local source=ctx.file:read_all(65536)
            local header,body=source:match("^(.-)\n(.*)$")
            assert(header and header:gsub("\r$","")=="PNOTES1","Expected PNOTES1 header.")
            local target=assert(ctx.project:track(1),"A first Track is required.")
            local writer=ctx.tx:note_writer(target)
            local batch={start={},gate={},key={},velocity={}}
            for line in body:gmatch("[^\n]+") do
                line=line:gsub("\r$","")
                local a,b,c,d=line:match("^(%d+),(%d+),(%d+),(%d+)$")
                assert(a,"Expected start,gate,key,velocity integer columns.")
                a,b,c,d=tonumber(a),tonumber(b),tonumber(c),tonumber(d)
                for _,v in ipairs({a,b,c,d}) do assert(math.type(v)=="integer","Integer overflow.") end
                local n=#batch.start+1
                batch.start[n]=a;batch.gate[n]=b;batch.key[n]=c;batch.velocity[n]=d
                if n==4096 then writer:append(batch);batch={start={},gate={},key={},velocity={}} end
            end
            if #batch.start>0 then writer:append(batch) end
            writer:finish()
        end,
        export=function(ctx)
            ctx.file:write("PNOTES1\n")
            local target=assert(ctx.project:track(1),"A first Track is required.")
            for batch in target:notes():batches{size=4096,columns={"start","gate","key","velocity"}} do
                local lines={}
                for i=1,batch.count do
                    lines[i]=string.format("%d,%d,%d,%d\n",batch.start[i],batch.gate[i],batch.key[i],batch.velocity[i])
                end
                ctx.file:write(table.concat(lines))
            end
        end}
end
```


Note Exchange streams16KiB inputs with128byte line carry and4096note batches; it caps
its demonstration at one million notes. Its100,000note native roundtrip was verified
under8MiB Lua memory, including a CRLF split between chunks and rollback after accepted
batches. Import review uses a zero-copy immutable native NoteStore; indexed overlap
query checkpoints cover rejected/off-row candidates too, bounded to64checkpoints
(at most262144candidate records, with metadata checkpoints also charged). Existing
20,000exact-note rendering/coverage aggregation and viewport/content/key-range cache
are reused. A dense preview may be incomplete and is labeled; the complete private
transaction remains the Apply result. Native App/worker tests are distinct from
interactive Windows, measured peak RSS/audio latency and final release acceptance.


### Authored fixed color-glyph MIDI example

The installable Text Art Studio v0.4 Color Glyph Laboratory demonstrates original
COLRv0-compatible glyf layers → native A8 → colorize → composite → one UI image.
Its explicit Capture for MIDI button serializes the current64squareRGBA as bounded
plain project plugin data. An independent named pixel preset can restore it without
font access. The Color Glyph Artwork tool reads only those fixed pixels; it does
not reopen a font or consult a mutable external image during Preview/Apply. Its Lua
chooses resizing/alpha/velocity/time/key mapping, reads `ctx.owner_track` Subchannels'
`note_color(velocity)` palette and sends the native image note plan to
`ctx.tx:note_writer(ctx.track):append_plan(plan,mapping)`. Native note colors, locks,
exact ticks/128–256/Clip-local coordinates and transactions remain authoritative.
See examples-lua/text-art-studio/color-laboratory.lua and main.lua for complete
capture/validation/preset/mapping code; no artistic algorithm is auto-generated by
the host. Capture is project settings outside Undo; generated notes have one normal
Undo/Redo boundary. Pending capture changes invalidate old previews before Apply.
