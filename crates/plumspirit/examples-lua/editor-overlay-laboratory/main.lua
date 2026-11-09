plum.require_api(0,2)
-- All geometry, mask colors, guide placement and readout formatting are authored Lua.
local guide
local auto_roll,auto_player=false,false
local function roll_view()
  return assert(plum.ui.view_snapshot("piano_roll"),"Show the Piano Roll and let it render before invoking this command.")
end
local function stage_guide(view)
  assert(guide,"Create or choose a guide image first.")
  plum.ui.publish_overlay(view,{id="image-guide",primitives={
    {kind="image",image=guide,x1=.15,y1=.15,x2=.85,y2=.85,color={255,255,255,96}},
    {kind="line",x1=.15,y1=.5,x2=.85,y2=.5,color={255,220,80,150}},
    {kind="text",x=.15,y=.15,text="Lua-authored image guide",color={255,230,110,220}}
  }})
end
local function publish_guide()
  stage_guide(roll_view())
  print("Image guide staged for the captured view; no notes were created.")
end
local function retain(source)
  local picture=plum.ui.image(source,{max_side=128,filter="nearest"})
  local fresh=plum.ui.retain_image(picture)
  if guide then guide:close()end
  guide=fresh;source:close()
end
plum.plugin.command{id="checker",name="Create Authored Checker Guide",run=function()
  roll_view()
  local rows={}
  for y=0,31 do local row={};for x=0,31 do
    local odd=(math.floor(x/4)+math.floor(y/4))%2==1
    row[#row+1]=string.char(odd and 255 or 30,odd and 210 or 80,odd and 80 or 180,255)
  end;rows[#rows+1]=table.concat(row)end
  retain(plum.images.from_rgba(32,32,table.concat(rows)));publish_guide()
end}
plum.plugin.command{id="choose_mask",name="Choose Image and Author Luminance Mask",run=function()
  roll_view()
  local file=plum.files.open{filters={{name="Images",extensions={"png","jpg","jpeg","bmp","gif","webp","tif","tiff"}}}}
  if not file then return end
  local source=plum.images.load(file);file:close()
  local info=source:info();local scale=math.min(1,128/math.max(info.width,info.height))
  local small=source:resize(math.max(1,math.floor(info.width*scale)),math.max(1,math.floor(info.height*scale)),"box");source:close()
  local mask=small:mask{kind="luminance",min=.5,preserve_alpha=true};small:close()
  local m=mask:info();local rows={}
  for y=0,m.height-1 do
    local row=mask:row(y);local rgba={}
    for x=1,#row do rgba[x]=string.char(255,210,80,string.byte(row,x))end
    rows[#rows+1]=table.concat(rgba)
  end
  mask:close();retain(plum.images.from_rgba(m.width,m.height,table.concat(rows)));publish_guide()
end}
plum.plugin.command{id="refresh",name="Refresh Guide for Current Roll View",run=publish_guide}
local function stage_player(view)
  local s=assert(plum.playback.snapshot())
  plum.ui.publish_overlay(view,{id="transport-readout",primitives={
    {kind="rect",x1=.02,y1=.08,x2=.65,y2=.15,fill=true,color={15,20,35,150}},
    {kind="text",x=.03,y=.1,text=string.format("%s | tick %d | %.3f BPM | %.3fx",s.state,s.tick,s.bpm,s.speed),color={255,220,100,255}}
  }})
end
plum.plugin.command{id="player",name="Stage Captured Player Transport Readout",run=function()
  stage_player(assert(plum.ui.view_snapshot("player"),"Show the MIDI Player first."))
  print("Captured Player readout staged; enable automatic refresh separately.")
end}
for _,surface in ipairs({"piano_roll","player"})do
  plum.plugin.command{id="clear_"..surface,name="Clear Laboratory "..surface.." Overlay",run=function()
    local view=assert(plum.ui.view_snapshot(surface),"Show the requested surface first.")
    plum.ui.clear_overlay(view,surface=="piano_roll" and "image-guide" or "transport-readout")
    if surface=="piano_roll" then auto_roll=false else auto_player=false end
  end}
end

-- Automatic refresh is an explicit author feature enabled only by these commands.
plum.plugin.command{id="auto_roll",name="Enable Automatic Guide Refresh",run=function()
  assert(guide,"Create or choose a guide image first.");stage_guide(roll_view());auto_roll=true
  print("Guide refresh enabled for subsequent native surface notifications.")
end}
plum.plugin.command{id="auto_player",name="Enable Automatic Player Readout",run=function()
  stage_player(assert(plum.ui.view_snapshot("player"),"Show the MIDI Player first."));auto_player=true
  print("Player readout refresh enabled for native surface/transport notifications.")
end}
plum.plugin.command{id="auto_off",name="Disable Laboratory Automatic Refresh",run=function()
  auto_roll=false;auto_player=false
end}
local function attempt(refresh)
  -- Visual feedback is best effort. Host final cancellation/lifecycle checks still
  -- discard the job; this catch only prevents a transient refusal pausing observers.
  local ok,err=pcall(refresh)
  if not ok then print("Overlay refresh skipped: "..tostring(err))end
end
plum.plugin.observe{id="surfaces",events={"surface_changed"},debounce_ms=75,run=function(event)
  if not event.data.visible then return end
  local surface=event.data.surface
  if (surface=="piano_roll" and auto_roll) or (surface=="player" and auto_player)then
    local view=assert(plum.ui.view_snapshot(surface))
    assert(view:info().view_revision==event.data.view_revision)
    attempt(function()if surface=="piano_roll" then stage_guide(view)else stage_player(view)end end)
  end
end}
plum.plugin.observe{id="transport",events={"transport_changed"},debounce_ms=100,run=function()
  if not auto_player then return end
  local view=plum.ui.view_snapshot("player")
  if view then attempt(function()stage_player(view)end)end
end}
function on_project_close()
  auto_roll=false;auto_player=false
  if guide then guide:close();guide=nil end
end
