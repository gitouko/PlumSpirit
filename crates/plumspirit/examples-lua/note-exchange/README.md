# Note Exchange

Install from Plugins → Lua Plugins → Examples, review the four requested capabilities
and enable the plugin. Choose Plugins → Note Exchange → Import Note Exchange (four
columns)… or Export Note Exchange (four columns)…. Choose a `.pnotes` file explicitly.
Native MIDI/LUP Open/Save behavior is unchanged.

The plugin itself defines the `PNOTES1` text format. Each subsequent nonblank line is
`start,gate,key,velocity` with decimal integer ticks, key0–127/255 and velocity1–127.
LF and CRLF are accepted; a final newline is optional. The first Track is the explicit
target. Import appends notes, exports that Track's own notes, and uses its native
channel/Subchannel defaults and colors. This intentionally small format does not store
Clips, CC, metadata, arbitrary per-note colors or a complete MIDI/native project.

Import is private until the native Review Import window's Apply. The currently opened
Track/Clip previews its staged result in the Piano Roll; the review lists resulting
Track note counts. Cancel changes nothing. Apply has one native Undo/Redo boundary and
refuses an expired project/plugin/revision. Finish other editing tools before starting.
Export is append-only private staging; the host publishes atomically after success.
Cancellation/failure keeps the previously selected destination intact.

The authored decoder reads16KiB chunks, carries at most128bytes for an incomplete line,
and sends4096note batches to the native writer. Export reads only four packed columns
and writes bounded text batches. The example caps either direction at one million
notes and keeps host execution/resource limits. It never materializes the whole file
or note query in Lua. Malformed data, out-of-range values, excessive lines and integer
overflow are explicit failures; earlier batches are still private and roll back.

Example:

```text
PNOTES1
0,96,60,100
120,96,64,100
9007199254741001,29,67,93
```

API0.2 development sample; native worker/App tests and actual interactive Windows
acceptance are recorded separately. No automatic converter algorithms are generated.
