plum.require_api(0, 2)
-- All recording choices, pairing and mathematical mappings below are authored Lua.
local KEY, PRESET = "midi_input_capture", "MIDI Input Laboratory Capture"
local MAX, POLLS = 256, 32
local FIELDS = {"status", "data1", "data2", "time", "driver_ms", "sequence", "received_ns", "out_of_order", "driver_overrun"}
local function integer(v, lo, hi)
  return math.type(v) == "integer" and v >= lo and v <= hi
end
local function valid(c)
  assert(type(c) == "table" and c.version == 1 and integer(c.count, 1, MAX), "Invalid capture schema/count.")
  assert(type(c.name) == "string" and #c.name <= 256 and type(c.truncated) == "boolean", "Invalid capture description.")
  for _, field in ipairs(FIELDS) do
    local a = c[field]; assert(type(a) == "table" and #a == c.count, "Invalid capture column: " .. field)
    local n = 0
    for k in pairs(a) do assert(integer(k, 1, c.count), "Invalid capture column index."); n = n + 1 end
    assert(n == c.count, "Capture columns must be dense.")
  end
  local sequence = 0
  for i = 1, c.count do
    local s, a, b = c.status[i], c.data1[i], c.data2[i]
    assert(integer(s, 0x80, 0xff) and integer(a, 0, 127) and integer(b, 0, 127), "Invalid captured MIDI bytes.")
    if s >= 0xf0 then
      assert((s == 0xf8 or s == 0xfa or s == 0xfb or s == 0xfc or s == 0xfe or s == 0xff) and a == 0 and b == 0, "Unsupported captured MIDI status.")
    elseif s & 0xf0 == 0xc0 or s & 0xf0 == 0xd0 then assert(b == 0, "Unused data byte must be zero.") end
    assert(integer(c.time[i], 0, plum.max_integer) and integer(c.driver_ms[i], 0, 4294967295)
      and integer(c.received_ns[i], 0, plum.max_integer) and integer(c.sequence[i], 1, plum.max_integer)
      and c.sequence[i] > sequence, "Invalid capture time/order.")
    assert(type(c.out_of_order[i]) == "boolean" and type(c.driver_overrun[i]) == "boolean", "Invalid capture flags.")
    sequence = c.sequence[i]
  end
  return c
end
local function stored()
  return valid(assert(plum.config.project.get(KEY), "Capture or load MIDI Input Laboratory data first."))
end
local function capture()
  assert(plum.project.current(), "Open a project first.")
  assert(#plum.midi_input.devices().devices > 0, "Windows reported no MIDI inputs; connect a device and invoke Capture again.")
  local input = plum.midi_input.open{capacity=1024}
  if not input then return end
  local c = {version=1, count=0, name=input:info().name, truncated=false}
  for _, field in ipairs(FIELDS) do c[field] = {} end
  for poll = 1, POLLS do
    local batch = input:read(math.min(64, MAX-c.count), 100)
    local s = batch.stats
    assert(s.status == "open", "MIDI input closed; previous capture is unchanged.")
    assert(s.queue_dropped + s.rate_dropped + s.invalid + s.contended + s.driver_overruns == 0,
      "Input discontinuity; previous capture is unchanged. Capture again with a lower event rate.")
    local b = batch.columns
    for i = 1, batch.count do
      local n = c.count + 1; c.count = n
      for _, field in ipairs(FIELDS) do c[field][n] = b[field == "time" and "timestamp_ms" or field][i] end
    end
    plum.progress(poll/POLLS, "Captured " .. c.count .. " MIDI messages")
    if c.count == MAX then c.truncated = true; break end
  end
  input:close()
  assert(c.count > 0, "No MIDI messages captured; previous capture is unchanged.")
  valid(c)
  plum.config.project.set(KEY, c)
  print("Stored " .. c.count .. " messages from " .. c.name .. "; live access is closed. Save the LUP project to retain this capture.")
end
local function replay(ctx)
  local c, p = stored(), ctx.params
  local origin = c.time[1]
  for i = 2, c.count do origin = math.min(origin, c.time[i]) end
  local columns = {start={}, gate={}, key={}, velocity={}}
  if p.channels then columns.channel = {} end
  local function tick(ms) return plum.ticks.add(p.start, plum.ticks.mul(plum.ticks.sub(ms, origin), p.scale)) end
  local function emit(ms, duration, key, velocity, channel)
    key = key + p.transpose
    if key < 0 or key >= ctx.keys then
      if p.outside == "Drop" then return end
      if p.outside == "Clamp" then key = math.max(0, math.min(ctx.keys-1, key))
      else error("Captured mapping is outside this project's key range; choose Clamp/Drop or change the parameters.") end
    end
    local n = #columns.start + 1
    columns.start[n] = tick(ms); columns.gate[n] = duration
    columns.key[n] = key; columns.velocity[n] = velocity
    if columns.channel then columns.channel[n] = channel end
  end
  if p.mode == "Notes" then
    -- FIFO pairing explicitly chosen by this author, including overlapping same-key
    -- messages. Raw timestamps and arrival sequence in the capture remain untouched.
    local held = {}
    local function finish(item, ms)
      local elapsed = math.max(1, ms-item.time)
      emit(item.time, plum.ticks.mul(elapsed, p.scale), item.key, item.velocity, item.channel)
    end
    for i = 1, c.count do
      local s, a, b, ms = c.status[i], c.data1[i], c.data2[i], c.time[i]
      local kind, channel = s & 0xf0, s & 15
      if kind == 0x90 and b > 0 then
        local id = channel*128+a; held[id] = held[id] or {}
        held[id][#held[id]+1] = {time=ms, key=a, velocity=b, channel=channel}
      elseif kind == 0x80 or (kind == 0x90 and b == 0) then
        local list = held[channel*128+a]
        if list and #list > 0 then finish(table.remove(list, 1), ms) end
      elseif s == 0xff or (kind == 0xb0 and (a == 120 or a == 123)) then
        for id = 0, 2047 do
          local list = held[id]
          if list and (s == 0xff or id//128 == channel) then
            for _, item in ipairs(list) do finish(item, ms) end; held[id] = nil
          end
        end
      end
    end
    -- A bounded recording can end with held notes. The author chooses a fixed
    -- fallback gate, rather than inventing an unseen future release time.
    for id = 0, 2047 do
      for _, item in ipairs(held[id] or {}) do emit(item.time, p.gate, item.key, item.velocity, item.channel) end
    end
  else
    for i = 1, c.count do
      local s, a, b = c.status[i], c.data1[i], c.data2[i]
      if p.mode == "CC Art" and s & 0xf0 == 0xb0 and a == p.controller then
        emit(c.time[i], p.gate, p.base + math.floor(b*p.span/127 + .5), p.velocity, s & 15)
      elseif p.mode == "Bend Ribbon" and s & 0xf0 == 0xe0 then
        local value = a + 128*b - 8192
        emit(c.time[i], p.gate, p.base + math.floor(value*p.span/8192 + .5), p.velocity, s & 15)
      end
    end
  end
  assert(#columns.start > 0, "No matching events in the captured data; choose another mapping or capture.")
  local writer = ctx.tx:note_writer(ctx.track)
  writer:append(columns); writer:finish()
  return string.format("Author mapping: %d captured messages → %d notes (%s).", c.count, #columns.start, p.mode)
end
function register(p)
  p.command{id="capture", name="Capture MIDI Input (up to 256 messages)", run=capture}
  p.command{id="inspect", name="Inspect Captured MIDI", run=function()
    local c = stored(); print(c.name .. ": " .. c.count .. " messages; " .. (c.truncated and "capture limit reached" or "bounded capture complete"))
  end}
  p.command{id="save", name="Save MIDI Capture Preset", run=function() plum.presets.save(PRESET, stored()) end}
  p.command{id="load", name="Load MIDI Capture Preset", run=function()
    assert(plum.project.current(), "Open a project first.")
    plum.config.project.set(KEY, valid(assert(plum.presets.load(PRESET), "Save a MIDI Input Laboratory Capture preset first.")))
  end}
  p.command{id="clear", name="Clear Captured MIDI", run=function() plum.config.project.remove(KEY) end}
  p.tool{id="replay", name="MIDI Input Laboratory — Captured Note Art", needs_selection=false,
    tooltip="Lua-authored FIFO note pairing, CC art and Bend Ribbon from a fixed capture; no live reads in Preview/Apply. Uses destination Track/Clip defaults and native Undo.",
    params={
      {key="mode", kind="choice", label="Author mapping", choices={"Notes","CC Art","Bend Ribbon"}, default="Notes"},
      {key="start", kind="ticks", label="Song / Clip-local start", min=0, max=plum.max_integer, default=0},
      {key="scale", kind="int", label="Ticks per captured millisecond", min=1, max=1024, default=2},
      {key="transpose", kind="int", label="Transpose mapping", min=-255, max=255, default=0},
      {key="base", kind="int", label="CC / Bend base key", min=0, max=255, default=60},
      {key="span", kind="int", label="CC / Bend key span", min=1, max=255, default=36},
      {key="controller", kind="int", label="CC Art controller", min=0, max=127, default=1},
      {key="gate", kind="ticks", label="Artwork / unfinished note gate", min=1, max=plum.max_integer, default=120},
      {key="velocity", kind="int", label="CC / Bend velocity", min=1, max=127, default=96},
      {key="outside", kind="choice", label="Outside project key range", choices={"Error","Clamp","Drop"}, default="Error"},
      {key="channels", kind="bool", label="Preserve captured MIDI channels", default=true},
    }, run=replay}
end
