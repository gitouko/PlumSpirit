plum.require_api(0,2)
local function send(options)
  local receipt=plum.playback.request(options)
  -- The acknowledgement describes the native owner's actual state, which can
  -- be audio_unavailable if the native device or SoundFont is not ready.
  plum.presets.save("Last transport control",{version=1,action=options.action,receipt=receipt})
  local s=receipt.snapshot
  local text=string.format("%s acknowledged: %s at tick %d, %.6fx.",options.action,s.state,s.tick,s.speed)
  print(text);plum.ui.notify(text)
end
for _,item in ipairs{{"play","Play"},{"pause","Pause"},{"stop","Stop"}} do
  local action,name=item[1],item[2]
  plum.plugin.command{id=action,name=name,run=function()send{action=action}end}
end
plum.plugin.command{
  id="seek",name="Seek to Exact Tick",
  params={{key="tick",label="Song tick",kind="ticks",min=0,max=9223372036854775807,default=0}},
  run=function(context)send{action="seek",tick=context.params.tick}end
}
plum.plugin.command{
  id="speed",name="Set Audition Speed",
  params={{key="speed",label="Native decimal speed",kind="text",max_len=64,default="1"}},
  run=function(context)send{action="speed",speed=context.params.speed}end
}

-- Loop is a persisted native project setting; stage it after this job releases
-- its immutable snapshot. This is not an immediate control acknowledgement.
local function stage_loop(options)
  local staged=plum.playback.stage_loop(options)
  plum.presets.save("Last staged loop",{version=1,intent=staged})
  print("Loop intent staged; native owner validates after this command finishes.")
end
plum.plugin.command{
  id="loop",name="Stage Loop Range",
  params={
    {key="start",label="Loop start tick",kind="ticks",min=0,max=9223372036854775807,default=0},
    {key="finish",label="Loop finish tick",kind="ticks",min=1,max=9223372036854775807,default=3840},
    {key="enabled",label="Enable loop",kind="bool",default=true}
  },
  run=function(context)stage_loop{start=context.params.start,finish=context.params.finish,enabled=context.params.enabled}end
}
plum.plugin.command{id="clear_loop",name="Clear Loop Range",run=function()stage_loop{clear=true}end}
