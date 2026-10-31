-- Development-only reference oracle for HZ-Chop (contract v0.8 §052).
-- Runs the ORIGINAL, unmodified "HZ-Chop for andromeda.lua" against a minimal
-- mocked Andromeda API and prints the generated notes, one case per block:
--   case <name> ppq=<ppq> bpm=<bpm> iNote=<n> fStep=<s>
--   <start> <length> <channel> <key> <velocity>      (one line per created note)
-- Usage: lua5.4 oracle.lua > ../../../crates/plumspirit_engine/tests/fixtures/hz_chop_golden.txt
-- Not shipped with PlumSpirit; needs Lua 5.4 (math.pow is shimmed: removed in 5.4).
math.pow = math.pow or function(a, b) return a ^ b end

local src = "../../../docs/references/v0_8/hz_chop/HZ-Chop for andromeda.lua"

local cases = {
  { name = "dynamic_step0_120bpm", ppq = 960, bpm = 120, iNote = -1, fStep = 0.0,
    notes = { {0, 960, 0, 60, 100}, {960, 1920, 3, 69, 90}, {4000, 37, 1, 81, 80},
              {5000, 1, 2, 21, 70}, {6000, 500, 9, 108, 127}, {7000, 3000, 0, 0, 1}, {11000, 200, 15, 127, 64} } },
  { name = "fixed45_step0_120bpm", ppq = 960, bpm = 120, iNote = 45, fStep = 0.0,
    notes = { {0, 960, 0, 60, 100}, {100, 333, 5, 30, 50} } },
  { name = "fixed0_step0", ppq = 960, bpm = 120, iNote = 0, fStep = 0.0,
    notes = { {0, 4000, 0, 60, 100} } },
  { name = "fixed127_step0", ppq = 960, bpm = 120, iNote = 127, fStep = 0.0,
    notes = { {10, 50, 0, 60, 100} } },
  { name = "dynamic_step_plus_half", ppq = 960, bpm = 120, iNote = -1, fStep = 0.5,
    notes = { {0, 2000, 0, 72, 100}, {3000, 700, 1, 50, 30} } },
  { name = "dynamic_step_minus_quarter", ppq = 960, bpm = 120, iNote = -1, fStep = -0.25,
    notes = { {0, 2000, 0, 40, 100}, {3000, 900, 1, 55, 30} } },
  { name = "dynamic_step_minus50_clamp", ppq = 960, bpm = 120, iNote = -1, fStep = -50.0,
    notes = { {0, 300, 0, 30, 100} } },
  { name = "dynamic_step_plus3", ppq = 960, bpm = 120, iNote = -1, fStep = 3.0,
    notes = { {0, 1500, 0, 64, 100} } },
  { name = "fraction_140bpm", ppq = 960, bpm = 140, iNote = -1, fStep = 0.0,
    notes = { {0, 3000, 0, 57, 100}, {5000, 2500, 2, 61, 80} } },
  { name = "fraction_133bpm_ppq480", ppq = 480, bpm = 133.33, iNote = -1, fStep = 0.0,
    notes = { {0, 2000, 0, 66, 100} } },
  { name = "low_ppq96", ppq = 96, bpm = 150, iNote = -1, fStep = 0.0,
    notes = { {0, 400, 0, 40, 100}, {500, 300, 0, 100, 90} } },
  { name = "fraction_step_mix", ppq = 960, bpm = 177, iNote = 50, fStep = 0.3,
    notes = { {0, 2500, 4, 70, 100} } },
  { name = "high_ppq_32767", ppq = 32767, bpm = 200, iNote = -1, fStep = -0.7,
    notes = { {0, 40000, 0, 69, 100} } },
}

for _, c in ipairs(cases) do
  -- mocked Andromeda API (constant tempo: ticks_to_secs(t) = t * 60 / (bpm * ppq))
  andromeda = {
    get_ppq = function(self) return c.ppq end,
    ticks_to_secs = function(self, t) return t * 60.0 / (c.bpm * c.ppq) end,
  }
  get_field_value = function(id) if id == "iNote" then return c.iNote else return c.fStep end end
  local created = {}
  local notes = {
    for_each_selected = function(self, f)
      for _, n in ipairs(c.notes) do
        f({ start = n[1], length = n[2], channel = n[3], key = n[4], velocity = n[5] })
      end
    end,
    create_note = function(self, start, length, channel, key, velocity)
      created[#created + 1] = { start, length, channel, key, velocity }
    end,
  }
  local P = dofile(src)
  P.on_apply(notes)
  print(string.format("case %s ppq=%d bpm=%s iNote=%d fStep=%s", c.name, c.ppq, tostring(c.bpm), c.iNote, tostring(c.fStep)))
  for _, n in ipairs(c.notes) do
    print(string.format("source %d %d %d %d %d", n[1], n[2], n[3], n[4], n[5]))
  end
  for _, n in ipairs(created) do
    print(string.format("%d %d %d %d %d", n[1], n[2], n[3], n[4], n[5]))
  end
end
