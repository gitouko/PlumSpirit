# Lua Plugins and Custom Tools

Community plugins are Lua 5.4 scripts that add **commands** and **custom note tools**. They
run inside PlumSpirit in a sandbox: no files, network or programs, bounded memory and time,
and nothing they did not ask for and you did not allow.

## Installing a plugin

1. Open {cmd:ShowLuaPlugins}.
2. **Folder…** (a folder with `plum-plugin.toml`) or **Lua File…** (one `.lua` file), or
   type the path and press **Install**. The plugin is copied into the plugins folder (`%APPDATA%\PlumSpirit\plugins\`, or
   Settings → Plugins → Plugins Folder).
3. Read what it asks for and tick each capability you allow: **read the project**, **edit
   notes**, **edit tracks**, **edit the Conductor**, **read / write a file you pick**.
4. Tick the box beside its name to enable it. Nothing is allowed or enabled by installing.

**Examples**: PlumSpirit ships two — **Graph Shift** (a note tool that shifts notes by a
graph) and **CC Batch** (controller curves and LFOs across the selection). Install them from
the window's Examples row, then read their `main.lua` in the plugins folder.

## Using plugins

* Commands: the **Plugins** menu → the plugin's name → the command. A command with **…** opens an
  options dialog first. While it runs, a bar above the status bar shows its progress with
  **Cancel**.
* Custom tools: **Tools → Custom** and the note menu → **Custom**. They preview live in the
  Piano Roll and apply as one Undo step, like built-in tools, with presets and shortcuts.
* Every change a plugin makes goes through a transaction: one Undo step, all or nothing. A
  plugin that fails changes nothing.

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
