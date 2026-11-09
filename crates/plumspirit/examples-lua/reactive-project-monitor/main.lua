-- All monitoring behavior below is authored in Lua.
plum.require_api(0, 2)
local recent, counts = {}, {}
local kinds = {"project_opened", "project_closed", "content_changed",
  "selection_changed", "tracks_changed", "clips_changed", "undo", "redo",
  "saved", "view_changed"}
local function record(e)
  assert(e.schema == 1 and e.context == "metadata" and e.read_only)
  counts[e.kind] = (counts[e.kind] or 0) + 1
  recent[#recent + 1] = {kind=e.kind, revision=e.revision, sequence=e.sequence}
  if #recent > 16 then table.remove(recent, 1) end
  local text = "OBS " .. e.kind .. " revision=" .. e.revision
    .. " coalesced=" .. e.coalesced .. " dropped=" .. e.dropped
  if e.data.notes then text = text .. " notes=" .. e.data.notes end
  if e.data.viewport then
    text = text .. " origin=" .. e.data.viewport.origin_tick
      .. " zoom=" .. e.data.viewport.pixels_per_tick
  end
  print(text)
  if e.kind == "saved" then plum.ui.notify("Observed successful " .. e.data.format .. " save.") end
end
function register(p)
  if plum.has("project.read") and plum.has("project.observe") then
    p.observe{id="monitor", events=kinds, debounce_ms=50, run=record}
  end
  p.command{id="summary", name="Show Observation Summary", run=function()
    local lines = {"Recent signals: " .. #recent .. "/16"}
    for _, kind in ipairs(kinds) do
      lines[#lines + 1] = kind .. ": " .. (counts[kind] or 0)
    end
    print(table.concat(lines, "\n"))
    return "Observation summary is in the plugin log."
  end}
end
function on_project_open()
  -- Legacy initialization is independent of observation registration timing.
  recent, counts = {}, {}
end
function on_project_close()
  -- Retain only bounded metadata for the summary command.
end
function on_unload()
  recent, counts = {}, {}
end
