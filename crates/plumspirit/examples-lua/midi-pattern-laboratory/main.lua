plum.require_api(0,2)
-- Pattern mathematics and musical choices belong to this example author.
-- The host supplies bounded plans, exact native time mapping and isolated voices.
plum.plugin.command{
  id="pattern",name="Stage Eight-Note Pattern",
  params={{key="root",label="Root MIDI key",kind="int",min=0,max=108,default=60}},
  run=function(context)
    local s=assert(plum.playback.snapshot(),"Open a project first.")
    assert(s.running,"Start native playback with a SoundFont first.")
    local columns={tick={},status={},data1={},data2={}}
    local function event(t,status,a,b)
      local i=#columns.tick+1
      columns.tick[i]=t;columns.status[i]=status;columns.data1[i]=a;columns.data2[i]=b
    end
    local intervals={0,4,7,12,7,4,0,19}
    event(plum.playback.tick_after(1.5),0xC0,0,0)
    for i,interval in ipairs(intervals)do
      local seconds=1.5+(i-1)*0.2
      local on=plum.playback.tick_after(seconds)
      local off=plum.playback.tick_after(seconds+0.15)
      event(on,0x90,context.params.root+interval,75+(i%3)*15)
      event(math.max(off,plum.ticks.add(on,1)),0x80,context.params.root+interval,0)
    end
    local writer=plum.playback.plan_writer{max_events=17}
    writer:append(columns)
    local plan=writer:finish()
    local staged=plan:stage{output="internal"}
    plan:close()
    plum.presets.save("Last authored pattern",{version=1,root=context.params.root,intervals=intervals})
    print("Pattern staged, not acknowledged as played; inspect playback.midi_output for native state.")
    assert(staged.staged)
  end
}
plum.plugin.command{id="cancel",name="Cancel My MIDI Stream",run=function()
  local result=plum.playback.cancel_midi()
  assert(result.cancellation_requested)
  print("Requested cancellation of this plugin's MIDI stream; song playback continues.")
end}
plum.plugin.command{id="state",name="Inspect Native MIDI State",run=function()
  local s=assert(plum.playback.snapshot(),"Open a project first.")
  print("Native plugin MIDI stream: "..s.midi_output)
end}
