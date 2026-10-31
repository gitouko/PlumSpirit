-- Graph Shift (example plugin, docs/LUA_API.md "Custom tools").
--
-- Tools > Custom > Graph Shift: each selected note moves up or down by
-- Shift x Shape(x) semitones, where x runs from 0 at the first selected
-- note's start to 1 at the last one's. Shape is edited in PlumSpirit's graph
-- editor. The window previews live in the Piano Roll; Apply commits exactly
-- the preview as one Undo step. Nothing here is random: the same notes and
-- settings give the same result every time.

-- The first and last start of the notes in query `q` (nil, nil without notes).
-- Notes come in stored order (by start), so one pass over the start column
-- is enough and memory stays bounded however many notes there are.
local function start_span(q)
  local first, last = nil, nil
  for b in q:batches{ size = 65536, columns = { "start" } } do
    if b.count > 0 then
      if first == nil then first = b.start[1] end
      last = b.start[b.count]
    end
  end
  return first, last
end

-- Rounds half up (deterministic for negative values too).
local function round(v)
  return math.floor(v + 0.5)
end

function register(p)
  p.tool{
    id = "graph-shift",
    name = "Graph Shift",
    tooltip = "Shifts each selected note up or down by a graph drawn over the selection's time span.",
    needs_selection = true,
    params = {
      { key = "amount", label = "Shift (Semitones)", kind = "int", min = -48, max = 48, default = 12,
        help = "How far a note moves where the graph is at its top. Negative values move notes down." },
      { key = "shape", label = "Shape", kind = "curve", default = { { 0, 0 }, { 0.5, 1 }, { 1, 0 } },
        help = "The shift over the selection: left = the first selected note, right = the last one; the top is the full Shift." },
      { key = "edge", label = "At the Keyboard's Edge", kind = "choice",
        choices = { "Clamp", "Keep Key", "Remove Note" }, default = "Clamp",
        help = "What happens to a note the shift would move past the lowest or highest key." },
    },
    run = function(ctx)
      local amount, shape, edge = ctx.params.amount, ctx.params.shape, ctx.params.edge
      local top = ctx.keys - 1
      local first, last = start_span(ctx.notes)
      if first == nil then return "No notes to shift." end
      local width = last - first
      -- Notes with equal starts get the same shift: remember the last one.
      local memo_start, memo_shift = nil, 0
      local moved = 0
      ctx.tx:modify_notes(ctx.notes, function(n)
        if n.start ~= memo_start then
          local x = width > 0 and (n.start - first) / width or 0
          memo_start, memo_shift = n.start, round(amount * shape.at(x))
        end
        if memo_shift == 0 then return nil end
        local key = n.key + memo_shift
        if key < 0 or key > top then
          if edge == "Keep Key" then return nil end
          if edge == "Remove Note" then moved = moved + 1 return false end
          key = key < 0 and 0 or top
          if key == n.key then return nil end
        end
        moved = moved + 1
        return { key = key }
      end)
      return string.format("Graph Shift changed %d notes.", moved)
    end,
  }
end
