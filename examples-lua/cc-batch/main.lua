-- CC Batch (example plugin, docs/LUA_API.md "Events" and "UI integration").
--
-- Plugins > CC Batch (Example) > Write CC Over Selection...: writes one
-- controller (CC) across the time span of the selected notes of the Piano
-- Roll's track (or, with no notes selected, the ruler's time selection):
-- a graph drawn in PlumSpirit's graph editor or an LFO (sine, triangle,
-- square, saw), every Step ticks, between two values. Repeated values are
-- left out, so a flat stretch costs one event. The whole batch is one
-- transaction, so it is one Undo step; the events are saved in the .lup
-- project and exported to MIDI like any other controller.
--
-- Clear CC Over Selection... removes that controller over the same span.

local MAX_EVENTS = 1000000

-- The span to work on: the selected notes' [first start, last end] on track
-- `t` and the first selected note's channel, or the ruler's time selection
-- and the track's default channel. Returns nil with a reason otherwise.
local function span(t)
  local q = plum.selection.notes(t)
  local first, last_end, channel = nil, nil, nil
  for b in q:batches{ size = 65536, columns = { "start", "gate", "channel" } } do
    for i = 1, b.count do
      local s, e = b.start[i], b.start[i] + b.gate[i]
      if first == nil then first, channel = s, b.channel[i] end
      if last_end == nil or e > last_end then last_end = e end
    end
  end
  if first ~= nil then return first, last_end, channel end
  local ts = plum.selection.time()
  if ts and ts.end_tick > ts.start_tick then
    return ts.start_tick, ts.end_tick, t:default_channel()
  end
  return nil, "Select notes in the Piano Roll or a time range on the ruler first."
end

-- 0 - 1 at position x (0 - 1) of the span.
local function shape_at(params, x)
  local shape = params.shape
  if shape == "Graph" then return params.graph.at(x) end
  local phase = params.cycles * x
  local f = phase - math.floor(phase)
  if shape == "Sine" then return 0.5 - 0.5 * math.cos(2 * math.pi * phase) end
  if shape == "Triangle" then return f < 0.5 and 2 * f or 2 - 2 * f end
  if shape == "Square" then return f < 0.5 and 0 or 1 end
  return f -- Saw
end

local function clamp_value(v)
  v = math.floor(v + 0.5)
  if v < 0 then return 0 end
  if v > 127 then return 127 end
  return v
end

local CONTROLLER = { key = "controller", label = "Controller", kind = "int", min = 0, max = 127, default = 11,
  help = "The CC number: 1 Modulation, 7 Volume, 10 Pan, 11 Expression, 64 Sustain, ..." }

function register(p)
  p.command{
    id = "write-cc",
    name = "Write CC Over Selection",
    tooltip = "Writes a controller curve or LFO across the selected notes' time span (or the time selection) as one Undo step.",
    params = {
      CONTROLLER,
      { key = "shape", label = "Shape", kind = "choice", choices = { "Graph", "Sine", "Triangle", "Square", "Saw" },
        default = "Graph", help = "Graph follows the graph below; the LFO shapes repeat LFO Cycles times and start at the low value." },
      { key = "graph", label = "Graph", kind = "curve", default = { { 0, 0 }, { 1, 1 } },
        help = "Used when Shape is Graph: left = the start of the span, right = its end; bottom = the low value, top = the high value." },
      { key = "cycles", label = "LFO Cycles", kind = "float", min = 0.25, max = 256, default = 4,
        help = "How many times an LFO shape repeats over the span." },
      { key = "values", label = "Values", kind = "range", min = 0, max = 127, default = { 0, 127 },
        help = "The low and high controller values." },
      { key = "step", label = "Step (Ticks)", kind = "ticks", min = 1, max = 3840, default = 60,
        help = "The distance between two events. Repeated values are left out." },
      { key = "replace", label = "Replace Existing", kind = "bool", default = true,
        help = "Removes this controller's events on the same channel inside the span first." },
    },
    run = function(ctx)
      local params = ctx.params
      local t = plum.selection.active_track()
      if not t then
        plum.ui.notify("Open a track in the Piano Roll first.", "warning")
        return
      end
      local s, e, channel = span(t)
      if s == nil then
        plum.ui.notify(e, "warning")
        return
      end
      local step, cc = params.step, params.controller
      local lo, hi = params.values.lo, params.values.hi
      local count = (e - s) // step + 2
      if count > MAX_EVENTS then
        error(string.format("That would write up to %d events; choose a larger Step.", count))
      end
      local list, last, n = {}, nil, 0
      local width = e - s
      local tick = s
      while true do
        local x = width > 0 and (tick - s) / width or 0
        local v = clamp_value(lo + (hi - lo) * shape_at(params, x))
        if v ~= last then
          n = n + 1
          list[n] = { kind = "cc", tick = tick, channel = channel, controller = cc, value = v }
          last = v
        end
        if tick >= e then break end
        tick = tick + step
        if tick > e then tick = e end
        if n % 65536 == 0 then plum.progress(width > 0 and (tick - s) / width or 1, "Building events") end
      end
      local tx = plum.transaction{ label = "Write CC " .. cc }
      local removed = 0
      if params.replace then
        removed = tx:delete_events(t:events{ kind = "cc", controller = cc, channel = channel,
          start_tick = s, end_tick = e + 1 })
      end
      tx:insert_events(t, list)
      tx:commit()
      if removed > 0 then
        return string.format("Wrote %d CC %d events (replaced %d).", n, cc, removed)
      end
      return string.format("Wrote %d CC %d events.", n, cc)
    end,
  }
  p.command{
    id = "clear-cc",
    name = "Clear CC Over Selection",
    tooltip = "Removes a controller's events (every channel) across the selected notes' time span or the time selection.",
    params = { CONTROLLER },
    run = function(ctx)
      local t = plum.selection.active_track()
      if not t then
        plum.ui.notify("Open a track in the Piano Roll first.", "warning")
        return
      end
      local s, e = span(t)
      if s == nil then
        plum.ui.notify(e, "warning")
        return
      end
      local cc = ctx.params.controller
      local tx = plum.transaction{ label = "Clear CC " .. cc }
      local removed = tx:delete_events(t:events{ kind = "cc", controller = cc, start_tick = s, end_tick = e + 1 })
      if removed == 0 then
        tx:cancel()
        return string.format("There was no CC %d to clear.", cc)
      end
      tx:commit()
      return string.format("Removed %d CC %d events.", removed, cc)
    end,
  }
end
