# Track List and Clips

Every project opens in the **Track List**: each track's clips on a timeline, beside the
track panel (the rows scroll together). {key:SwitchView} or the toolbar's **Track List /
Piano Roll** buttons switch the central area.

{fig:track-list-clips}

## Working with clips

| Action | How |
|---|---|
| Select | Click a clip; Ctrl+click adds or removes; Shift+click selects along the row; drag on empty space for a box; {key:SelectAll} selects every clip; Esc selects nothing |
| Move | Drag a clip: the selected clips move together, also to other tracks' rows (Snap applies) |
| Nudge | {key:NudgeClipsLeft} / {key:NudgeClipsRight}: the selected clips one Snap step earlier / later (one beat with Snap off), never before the song's start |
| Resize / trim | Drag a clip's right edge (its length) or left edge (trims the start; notes stay where they play) |
| Add a clip | Right-click a track's empty timeline → **Add Piano Roll Clip Here**, or {key:AddClip} at the edit cursor |
| Open in the Piano Roll | Double-click, or {key:OpenClip} |
| Duplicate | Alt+drag (linked copies where you drop them), or {key:OpenTool("duplicate")} right after the selection |
| Slice | {key:SliceNotes} slices the selected clips at the edit cursor (two clips showing the same notes) |
| Copy / Paste | {key:Copy} / {key:Paste}: independent copies on the active track at the edit cursor, including from another PlumSpirit window on Windows. {key:Cut} copies before deleting |
| Paste Repeat | {key:PasteRepeat}: the selected clips again a number of times, end to end or evenly spaced |
| Merge | {cmd:MergeClips(Sequence)} or {cmd:MergeClips(Stack)} — a Preview first |
| Delete | {key:Delete} |
| Zoom to the selection | {cmd:ZoomToSelection}: the selected clips across the Track List (in the Piano Roll: the selected notes) |
| Find the playhead | {cmd:JumpToPlayhead}: scrolls to the playhead and resumes a paused Follow Playhead |

While the Track List is shown these keys act on the selected clips, never on the Piano
Roll's notes (Slice cuts clips, not notes); with no clip selected a key says so in the
status bar. Change any of them in Help → Keyboard Shortcuts.

Right-click a clip for its menu: Open in Piano Roll, Copy / Cut, Slice, Duplicate
(Linked / Independent Copy), Paste Repeat…, Nudge Right / Left, Zoom to Selection, Mute /
Unmute, Rename…, Clip Color…, Merge, Delete, and the track's own menu. Every clip edit is
one Undo step.

## Linked and independent clips

A clip marked **∞** shares its notes with other clips: editing one edits all of them. An
independent copy has its own notes. A muted clip is shown faded and hatched; it does not
play, export or count.

## How clips look

A clip shows its notes in miniature — exactly while there are few, as a color-aware picture
when there are millions — never blank. Settings → Notes / Colors → **Display Note Colors** draws
them in their note colors; Settings → Track List → **Clip Body Transparency** sets how see-through the
clip body is.

## Editing a clip

Opening a clip shows its notes in the Piano Roll; the corner says which clip you edit. The
ruler, grid, markers and playhead stay in song time. Every note edit changes that clip's
notes only, and linked copies show it at once.

The Progressor and Selection Analytics also work over the Track List (Selection Analytics
then describes the selected clips). Scroll with the wheel, Shift+wheel for time,
Ctrl+wheel to zoom.

See also [Tracks, Subtracks, Folders and Subchannels](#tracks), [Editing Notes in the
Piano Roll](#piano-roll).
