# Progressor and Selection Analytics

## Progressor

{cmd:ToggleProgressor} shows a readout in the corner of the Piano Roll (and over the Track
List) for the playhead — or the edit cursor when stopped:

* **Notes** — notes started so far / all notes;
* **NPS** — notes starting in the last second of song time;
* **Poly** — notes sounding right now (stacked notes count separately);
* **PPS** — the average poly over the last second.

The numbers are exact at any position, also after a seek, while paused and in loops, and
follow every edit (after a very large edit they are dimmed for a moment with "Updating
analysis…"). {cmd:ToggleProgressorMuteSolo} (on by default) counts only what you hear;
the first line says which (Audible Tracks / All Tracks).

{cmd:ProgressorSettings} chooses the fields, the text itself (any words, with `{nps}`,
`{poly}`, `{notes}` … replaced by values; `{{` writes a brace), the corner, font size,
colors, opacity, background, number format (40,000,000 / 40000000 / 40.0M), which notes
count and the window. These are application settings, never saved in a project.

## Selection Analytics

{cmd:ToggleSelectionAnalytics} describes the selected notes exactly: how many, their span,
NPS and PPS, peak NPS / PPS / poly and where they are first reached, unique-pitch poly,
poly area, average NPS and poly, and the gate range (shortest ~ longest note, as note values
and beats). In the Track List it describes the selected clips' notes as they play.
{cmd:SelectionAnalyticsSettings} edits its text like the Progressor's (with fields such as
`{selected}`, `{peak_nps}`, `{peak_nps_at}`, `{duration}`).

## Live keyboard and key numbers

With {cmd:ToggleLiveKeys}, every key with notes sounding on it shows the color of the note
in front, at the playhead — by default among the tracks the Piano Roll shows (the active
track, and the Onion ghosts while the Onion Skin is on). Hover a key for the count of
sounding notes.

The readouts fade while the pointer is over them; their corner arrow collapses or moves
them, and {cmd:ToggleTextOverlays} hides them.
