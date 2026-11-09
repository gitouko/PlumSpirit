plum.require_api(0, 2)
local function load()
  local value, version = plum.metadata.project()
  if version == nil then return {visits=0, points={{x=0,y=0},{x=1,y=1}}}, 0 end
  assert(version <= 2, "This notebook was written by a newer plugin; leaving it untouched.")
  assert(type(value) == "table", "Notebook data must be a table.")
  if version == 1 then
    -- The author defines the migration. The host never guesses a schema.
    value = {visits=value.old_visits or 0, points=value.points or {{x=0,y=0},{x=1,y=1}}}
  end
  assert(math.type(value.visits) == "integer" and value.visits >= 0 and value.visits < 1000000)
  return value, version
end
function register(p)
  p.command{id="remember", name="Remember Project Attributes", run=function()
    local project = plum.project.current()
    assert(project, "Open a project first.")
    local value, version = load()
    value.visits = value.visits + 1
    value.title = project:name()
    value.key_rows = project:key_range()
    value.exact_tick = 9007199254740993
    local tx = plum.transaction{label="Metadata Notebook"}
    tx:set_project_metadata(value, {version=2, expect_version=version})
    tx:commit()
    print("Notebook visit " .. value.visits .. "; save the LUP project to persist it.")
  end}
  p.command{id="remember-track", name="Remember First Track Attributes", run=function()
    local project = assert(plum.project.current(), "Open a project first.")
    local track = assert(project:track(1), "The project needs a Track.")
    local old, version = plum.metadata.get(track)
    assert(version == nil or version <= 1, "Newer Track schema; leaving it untouched.")
    local visits = old and old.visits or 0
    assert(math.type(visits) == "integer" and visits >= 0 and visits < 1000000)
    local tx = plum.transaction{label="Track Notebook"}
    tx:set_metadata(track, {visits=visits+1, title=track:name(), exact_tick=9007199254740993},
      {version=1, expect_version=version or 0})
    tx:commit()
  end}
  p.command{id="clear-track", name="Clear First Track Attributes", run=function()
    local project = assert(plum.project.current(), "Open a project first.")
    local track = assert(project:track(1), "The project needs a Track.")
    local _, version = plum.metadata.get(track)
    if version == nil then return end
    local tx = plum.transaction{label="Clear Track Notebook"}
    tx:remove_metadata(track, {expect_version=version})
    tx:commit()
  end}
  p.command{id="remember-group", name="Remember First Group Attributes", run=function()
    local project = assert(plum.project.current(), "Open a project first.")
    local group = assert(project:groups{limit=1}[1], "Create a permanent native Group first.")
    local old, version = plum.metadata.get(group)
    assert(version == nil or version == 1, "Newer Group schema; leaving it untouched.")
    local visits = old and old.visits or 0
    assert(math.type(visits) == "integer" and visits >= 0 and visits < 1000000)
    local tx = plum.transaction{label="Group Notebook"}
    tx:set_metadata(group, {visits=visits+1, purpose="Author-defined group annotations"},
      {version=1, expect_version=version or 0})
    tx:commit()
  end}
  p.command{id="clear-group", name="Clear First Group Attributes", run=function()
    local project = assert(plum.project.current(), "Open a project first.")
    local group = assert(project:groups{limit=1}[1], "Create a permanent native Group first.")
    local _, version = plum.metadata.get(group)
    if version == nil then return end
    local tx = plum.transaction{label="Clear Group Notebook"}
    tx:remove_metadata(group, {expect_version=version})
    tx:commit()
  end}
  p.command{id="inspect", name="Inspect Project Attributes", run=function()
    local value, version = plum.metadata.project()
    print(version and ("Notebook schema " .. version .. ", visits " .. tostring(value.visits)) or "No notebook in this project.")
  end}
  p.command{id="clear", name="Clear Project Attributes", run=function()
    local _, version = plum.metadata.project()
    if version == nil then return end
    local tx = plum.transaction{label="Clear Metadata Notebook"}
    tx:remove_project_metadata{expect_version=version}
    tx:commit()
  end}
end
