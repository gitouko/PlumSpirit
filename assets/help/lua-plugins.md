# Lua Plugins and Custom Tools

PlumSpirit v1.0.1 supports Lua API 0.2.0 and existing API 0.1 plugins.

Community plugins are Lua 5.4 scripts that add **commands** and **custom note tools**. They
run inside PlumSpirit with bounded memory and time. Plugins receive only the capabilities
you grant. Files require your selection; there is no unrestricted filesystem, network or
program access.

## Installing a plugin

1. Open {cmd:ShowLuaPlugins}.
2. **Folder…** (a folder with `plum-plugin.toml`) or **Lua File…** (one `.lua` file), or
   type the path and press **Install**. The plugin is copied into the plugins folder (`%APPDATA%\PlumSpirit\plugins\`, or
   Settings → Plugins → Plugins Folder).
3. Read what it asks for and tick each capability you allow: **read the project**, **edit
   notes**, **edit tracks**, **edit the Conductor**, **read / write a file you pick**.
4. Tick the box beside its name to enable it. Nothing is allowed or enabled by installing.

**Examples**: PlumSpirit ships 22 authored examples, including the original **Graph Shift**
and **CC Batch**, **Custom Graph Laboratory**, **Custom Drawing Studio**, **Image Art Studio**
and **Text Art Studio**. Install them from the window's Examples menu, then read their
`main.lua` in the plugins folder. Installing an example grants no capabilities.

## Using plugins

* Commands: the **Plugins** menu → the plugin's name → the command. A command with **…** opens an
  options dialog first. While it runs, a bar above the status bar shows its progress with
  **Cancel**.
* Custom tools: **Tools → Custom** and the note menu → **Custom**. They preview live in the
  Piano Roll and apply as one Undo step, like built-in tools, with presets and shortcuts.
* Drawing tools capture a stroke in the Piano Roll, then process it on a worker. Preview
  the notes before Apply; Escape cancels the active stroke. Existing native Draw/Paint and
  the original curve editor remain available.
* Musical edits go through transactions and the existing Undo history. Failed processing
  discards private staging. Saving a selected external file is a separate explicit action
  outside musical Undo; check the plugin's description before running it.
* API 0.2 plugins may provide custom panels, graphs, image/text artwork, editor overlays,
  format adapters, reactive observers or composed services. Playback and live MIDI input
  need their own capabilities; each MIDI input device also needs explicit approval.

## Managing plugins

The window lists each plugin's state, version, origin and **Log**. **Update from Path**
replaces a plugin with a newer package; **Remove…** deletes it; **Reload** starts it again; **Refresh** reloads plugins whose files changed
(a failing new version leaves the old one running). **Copy for a Bug Report** copies the log
with your user name and folders removed. {cmd:ToggleRunLuaPlugins} switches every plugin
off without forgetting anything.

> Plugins run inside PlumSpirit's process. Install only plugins you trust; PlumSpirit
> shows where a plugin came from and what it asks for before it can run, and never installs
> anything from the internet by itself.

Writing plugins: `docs/LUA_API.md` next to PlumSpirit.exe (the API reference, examples and
a compatibility checklist).
