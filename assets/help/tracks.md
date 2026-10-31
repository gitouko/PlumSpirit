# Tracks, Subtracks, Folders and Subchannels

## Tracks

The track panel lists the **Conductor** first, then your tracks. {cmd:AddTrack} adds one;
{cmd:AddTracks} adds as many as you type in one step (one Undo): below the active track at
its level (in its folder), or at the end when the Conductor is selected. A track's menu has
**Add Tracks Below…**, a folder's **Add Tracks to Folder…**, the empty space **Add
Tracks…** (at the end). The dialog says where they go, how many tracks the project will have
and the memory they take, and asks once more from 1,000 tracks on; a project holds at most
65,535 tracks (the limit of a MIDI file). New tracks have no clips yet, so the Piano Roll
stays read-only on them until you add one. Double-click a track (or {cmd:TrackProperties}) for its properties: name, output (the
internal synthesizer or a MIDI device), the MIDI channel of new notes, Melody / Rhythm,
**corrections** (tick shift, transposition, gate percentage — they change how it plays and
exports, never the stored notes), a key range, its color and the track end.

**M** and **S** mute and solo a track (Ctrl+click S to solo several; Mute wins over Solo, so a
muted track's S is not lit). They change what you hear, never the export. {cmd:PlayAll} clears
them. The larger **M** / **S** above the list ({cmd:MuteCurrentTrack}, {cmd:SoloCurrentTrack})
act on the current track.

{cmd:PlayOnlySelected} plays exactly the selected tracks (or the current one; in the Track List,
the tracks of clips selected on several tracks) without changing any Mute or Solo — nothing is saved; {cmd:RestoreAllTracks} plays the song as it was again.
While it is on, those tracks play whatever their Mute / Solo; the Progressor, the keyboard
colors and the external outputs follow, Onion Skin and editing are unchanged.

{cmd:MoveTrackUp}, {cmd:MoveTrackDown}, or drag tracks to reorder them.
{cmd:DeleteSelectedTracks} asks first.

## Subtracks

A subtrack is a track inside another track. {cmd:AddSubtrack} or {cmd:AddSubtracks};
{cmd:IndentTrack} and {cmd:OutdentTrack} move a track into or out of the one above it. The
▸ / ▾ arrow folds a parent — only the view: folded subtracks still play and export. MIDI
export writes the tree in order with data that restores it when PlumSpirit opens the file.

## Folders

Folders group top-level tracks for a tidy panel. A folder has no notes, does not play and
is never exported. Right-click a track → **Folder** → New Folder with This Track / with
Selected Tracks / New Empty Folder Here / Remove from Folder. Click a folder to fold it,
double-click to rename, drag to move it with its tracks.

## Subchannels

A **subchannel** is a colored group of notes inside a track — PlumSpirit's own
organization, not a MIDI channel. A track can have any number of them; each has the MIDI
channel its new notes get, and several may share one.

* The **Subchannel** strip under the track panel shows the current subchannel (new notes go
  there), its MIDI channel, swatches for the first 16 and **+** to add one.
* {cmd:Subchannels} lists them all with note counts: edit, select their notes, delete.
* {cmd:MoveToSubchannel} regroups selected notes; {cmd:PaintCurrentSubchannel} paints them
  with the current one.

## Channels

Tracks are not channels: every note keeps its own MIDI channel (1 – 16). Channel 10 is the
General MIDI drum channel; **Song → New Subchannels Skip MIDI Channel 10** keeps new
subchannels off it.

See also [Track List and Clips](#track-list), [Tempo, Time Signatures and
Markers](#conductor).
