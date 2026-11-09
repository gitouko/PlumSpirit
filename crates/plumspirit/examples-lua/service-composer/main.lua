plum.require_api(0, 2)
local service = "plumspirit.example.note-service/"
local function first_track()
  local project = plum.project.current()
  assert(project, "Open a project first.")
  return project, assert(project:track(1), "The first Track is unavailable.")
end
function register(p)
  p.command{id="generate", name="Generate Composed Service Pattern", run=function()
    local project, track = first_track()
    local reply = plum.services.call(service .. "pattern", {count=16, start=0,
      spacing=120, gate=96, base_key=60, velocity=90, max_key=project:key_range()-1}, {seed=12345})
    local tx = plum.transaction{label="Composed Service Pattern"}
    tx:insert_notes(track, reply.notes)
    tx:commit()
  end}
  p.command{id="transform", name="Transpose Selected Notes via Service", run=function()
    local project, track = first_track()
    local query = track:notes{selected=true}
    local count = query:count()
    assert(count <= 2048, "This bounded example processes at most 2,048 selected notes.")
    if count == 0 then return "Select notes on the first Track first." end
    local notes = {}
    for columns in query:batches{size=32, columns={"start", "gate", "key", "velocity",
      "channel", "subchannel", "muted", "off_kind", "off_velocity", "group"}} do
      local reply = plum.services.call(service .. "transform", {columns=columns,
        transpose=7, max_key=project:key_range()-1})
      for _, row in ipairs(reply.notes) do notes[#notes + 1] = row end
    end
    -- Stage only after every service call succeeds; locks remain authoritative.
    local tx = plum.transaction{label="Composed Service Transpose"}
    tx:delete_notes(query)
    tx:insert_notes(track, notes)
    tx:commit()
  end}
end
