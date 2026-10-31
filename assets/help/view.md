# Zoom, Onion Skin, Colors and Keys

## Zoom and scrolling

| Action | How |
|---|---|
| Scroll keys / time | Wheel / Shift+wheel, or the scrollbars |
| Zoom time around the pointer | Ctrl+wheel (or a two-finger trackpad scroll with Ctrl, or a pinch on a Precision Touchpad), or the toolbar's − / + beside X |
| Zoom keys (Piano Roll) / row height (Track List) around the pointer | Ctrl+Shift+wheel or a two-finger trackpad scroll with Ctrl+Shift (smooth, fractional), or {cmd:ZoomInY} / {cmd:ZoomOutY} |
| Pan | Middle-mouse drag |
| Exact zoom | Type a percentage in the toolbar's X / Y field and press Enter |
| Whole song | {cmd:FitSong}; {cmd:ResetZoom} goes back to 100 % |
| All keys in the height | {cmd:FitKeys128} (and {cmd:FitAllKeys} in 256-Key Mode) |

Settings → Piano Roll → **Default Y View** = Fit All Keys opens every project (and Reset
View) with all 128 keys (256 in 256-Key Mode) in the height. **Lock Fit Range** keeps fitted
keys fitted when the window changes;
**Y Zoom Beyond the Keys** (same place) lets you zoom out past the keyboard. {cmd:ZoomTool},
{cmd:ZoomBack} and {cmd:ZoomForward} move between views. **Follow Playhead** keeps the
playhead in view while playing (Settings → Playback & External MIDI → **Follow Position**,
**Smooth Follow**). Smooth Follow keeps the playhead at the follow position — at 100 % just
inside the right edge — in the Piano Roll and the Track List alike.

Check boxes in the menus toggle without closing the menu; Esc or a click outside closes it.
The View menu holds view actions only — the main view, panels and readouts, Follow, Onion
Skin, zoom and fit; its last item, **View → View Settings…**, opens Settings, where every
display option lives once.

## Onion Skin

{fig:onion-skin}

Onion Skin draws other tracks' notes as faint **ghosts** behind the track you edit; they can
never be selected or edited. {cmd:ToggleOnion} switches it on and off; the toolbar's Onion
button's ▾ (and **View → Onion Skin**) chooses Previous / Next / Specific / All Other
Tracks, the layer order (project order, densest or sparsest first), {cmd:OnionOpacity} and
{cmd:ToggleOnionOutlines} with their darkness. In the track panel, each track's color box
is its Onion switch: click it (added to the other ghosts), Alt+click it (only that track),
or drag over several. For the tracks selected in the track list: {cmd:OnionShowOnlySelected}
replaces every ghost — an earlier list does not stay — {cmd:OnionAddSelected} and
{cmd:OnionRemoveSelected} keep the others, {cmd:OnionSelectedTracks} shows or hides them.
**Per-Track Settings** lets each track keep its own; {cmd:OnionUseForEveryTrack} makes the
setting in force every track's (their own are reset).

## Colors

* Louder notes are more saturated; each subchannel has its own hue.
* Settings → Notes / Colors → **General Color Preferences…**: random by seed (seed 0 is PlumSpirit's palette), Rainbow Through 16 Subchannels, or
  your own colors for subchannels 1 – 16.
* Every track has a **Track color** (24 pigment-named colors, or your own in Track
  Properties), used by its ghosts, its clips and the keyboard.
* {cmd:SelectionOpacity} sets how strongly selected notes stand out.
* Settings → Appearance → **Theme**: Dark, Light or System, and {cmd:ToggleHighContrast}.
* Settings → Interface & Accessibility → **UI Scale**: 30 – 175 % of the system size;
  {key:ResetUiScale} resets it.

## The keyboard

* {cmd:ToggleLiveKeys}: while playing, each key shows the color of the note in front on it —
  by default of what the Piano Roll shows: the active track (and its subtracks), plus the
  Onion ghosts while the Onion Skin is on; with it off, no other track colors a key
  (Settings → Piano Roll → **Keyboard Colors Show** chooses all / audible tracks instead).
* {cmd:ToggleAudition}: press a key to hear it; drawn notes play briefly.
* {cmd:ToggleKeyGuide} marks the 88 keys of a piano (A0 – C8).
* Hover a key for its number and name (`Key 60 · C4`).

## 256-Key Mode

In {cmd:ProjectProperties}: the project stores keys 0 – 255. Keys above 127 are silent (no
synthesizer can play them) and a standard MIDI file cannot hold them, so Export asks what to
do with them.

## Readouts over the notes

The Progressor and Selection Analytics text fades while the pointer is over it. Its corner
arrow collapses it (click) or moves it (drag). {cmd:ToggleTextOverlays} hides both without
turning them off.
