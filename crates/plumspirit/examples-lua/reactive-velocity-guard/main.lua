-- Community authors own both the observation rule and the musical algorithm.
plum.require_api(0, 2)
local ceiling = 100
local function clamp(ctx)
  local project = plum.project.current()
  if not project then return end
  local track = project:track(1)
  if not track then return end
  local limit = ctx.request and ctx.request.ceiling or ceiling
  assert(math.type(limit) == "integer" and limit >= 1 and limit <= 126)
  local query = track:notes{velocity={limit + 1, 127}}
  if query:count() == 0 then return end
  local tx = plum.transaction{label="Reactive Velocity Guard"}
  local changed = tx:modify_notes(query, {velocity=limit})
  tx:commit()
  print("Velocity Guard adjusted " .. changed .. " notes.")
end
function register(p)
  p.command{id="clamp", name="Clamp First Track Velocity", run=clamp}
  if plum.has("project.read") and plum.has("project.observe")
    and plum.has("project.automate") and plum.has("notes.edit") then
    p.observe{id="velocity", events={"content_changed"}, debounce_ms=75, run=function(e)
      if e.transaction_allowed then
        p.request_transaction{command="clamp", data={ceiling=ceiling}}
      end
    end}
  end
end
