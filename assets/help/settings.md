# Settings and Themes

{cmd:OpenSettings} (or the gear at the left of the toolbar, or **View → View Settings…**)
opens one window with every option, each in one place, in twelve sections — **Appearance**,
**Interface & Accessibility**, **Piano Roll**, **Track List**, **Playback & External MIDI**,
**Rendering / Onion**, **Notes / Colors**, **Import / Export**, **Performance / Memory**,
**Plugins**, **Shortcuts** and **Advanced Diagnostics** — and a **Search** over the names and
descriptions. Changes apply at once. **Reset section** and **Restore all defaults** bring the
defaults back; projects are never changed by them. Options with their own window (Audio
Settings, General Color Preferences, the Progressor, the MIDI Player…) open it from here. The
View menu keeps only view actions (the main view, panels, readouts, Follow, Onion Skin, zoom
and fit).

A few to know:

* **Appearance** — theme, **UI Accent Color**, **High Contrast**.
* **Interface & Accessibility** — **UI Scale** (30 – 175 %; type any whole percent;
  {key:ResetUiScale} resets it), Hide Text Overlays, the Progressor and Selection Analytics
  readouts, their texts and the selection charts.
* **Piano Roll** — the default zoom of a new view, Event Graphs, the 88-key guide, Lock Fit
  Range, Y Zoom Beyond the Keys, Preview Batch Edits, Scale Y and overlap prevention, the
  rotation pivot, Live Keyboard Colors, Keyboard Colors Show, Note Audition.
* **Track List** — Clip Body Transparency.
* **Playback & External MIDI** — Follow Playhead, Smooth Follow, Follow Position, the Edit /
  Anchor playhead, After Pause, Audio Settings…, the SoundFont, the MIDI Player and Inspector.
* **Rendering / Onion** — the renderer (OpenGL or Vulkan) and the one in use, the GPU,
  Rendering Quality (Fast / Balanced / Detailed), ghost outlines, the Onion Skin of new
  projects (the one you last used, or off) and its layer order.
* **Notes / Colors** — General Color Preferences…, Display Note Colors, Selection Opacity.
* **Import / Export** — what New Project starts with (BPM, PPQN, 256-Key Mode), Safe Saving,
  automatic backups, running status in exported MIDI, Program Change 0 for players that need
  a control event, a silent loudness reference for Piano From Above.
* **Performance / Memory** — the tile cache and preload distance.
* **Plugins** — Lua Plugins…, Run Lua Plugins, Enable Plugins When Installed, the plugins
  folder, Audio Reference….
* **Shortcuts** — {cmd:ShowKeybinds}; every shortcut can be changed, exported and imported.
* **Advanced Diagnostics** — CPU / RAM in the status bar, the diagnostics reports (PlumSpirit
  sends nothing over the network).

## General Color Preferences

Notes / Colors → **General Color Preferences…** opens separate **Note Color Preferences**
and **Track Color Preferences** panels. Existing note modes and saved manual colors
are retained. Track Properties overrides remain separate from the automatic palette.

## UI Accent Color

Pick a color in Settings → Appearance, or type a six-digit hex color such as
`#00CC88` and press Enter. Focus rings, active controls, tabs and selection frames
update at once. Light and dark shades adjust for contrast. **Reset Color** restores
the original purple in each theme; Reset section and Restore all defaults also
restore it. The color is kept for the next start.

Note colors, Track colors, velocity saturation, warnings and MIDI exports keep
their own colors and settings.

## Where settings live

Settings are kept in `%APPDATA%\PlumSpirit\settings.json`, with the log
(`plumspirit.log`), the undo histories, backups, plugins and presets in the same folder. They
never travel inside a project: Mute / Solo, the loop, the Onion Skin and the view do, settings
do not (a new project or MIDI file starts with the Onion Skin you last used). The full table
is in `docs/SETTINGS_OWNERSHIP.md`.

## Help text size

This window's **A−** / **A+** change the size of the Help text only (80 – 200 %); UI Scale
changes everything.
