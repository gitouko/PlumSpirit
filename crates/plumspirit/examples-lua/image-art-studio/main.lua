plum.require_api(0,2)
-- All artwork choices, palettes and editing behavior below are authored Lua.
-- The host supplies bounded images, masks, native transactions and tool Preview.
local CAPTURE_KEY="image_art_capture"
local PRESET="Image Art Studio Capture"
local function capture_valid(s)
  return type(s)=="table" and s.version==1 and type(s.width)=="number" and s.width%1==0
    and type(s.height)=="number" and s.height%1==0 and s.width>=1 and s.width<=64
    and s.height>=1 and s.height<=64 and type(s.hex)=="string"
    and #s.hex==s.width*s.height*8 and not s.hex:find("[^0-9a-fA-F]")
end
local function default_image()
  local rows={}
  for y=0,15 do local row={}
    for x=0,15 do
      -- An explicit striped/checkered author algorithm, not a host effect.
      local band=(math.floor(x/4)+math.floor(y/4))%3
      local alpha=(x+y)%7==0 and 0 or 255
      row[#row+1]=string.char(band==0 and 255 or 0,band==1 and 255 or 0,band==2 and 255 or 0,alpha)
    end
    rows[#rows+1]=table.concat(row)
  end
  return plum.images.from_rgba(16,16,table.concat(rows))
end
local function source_image()
  assert(plum.project.current(),"Open a project first.")
  local capture=plum.config.project.get(CAPTURE_KEY)
  if capture==nil then return default_image() end
  assert(capture_valid(capture),"Invalid saved capture. Reset the source or choose an image again.")
  local bytes=capture.hex:gsub("..",function(pair)return string.char(tonumber(pair,16))end)
  return plum.images.from_rgba(capture.width,capture.height,bytes)
end
local function choose_source()
  assert(plum.project.current(),"Open a project first.")
  local file=plum.files.open{filters={{name="Image",extensions={"png","jpg","jpeg","bmp","gif","tif","tiff","webp"}}}}
  if not file then return end
  local original=plum.images.load(file)
  file:close()
  local info=original:info()
  local scale=math.min(1,64/info.width,64/info.height)
  local width=math.max(1,math.floor(info.width*scale+.5))
  local height=math.max(1,math.floor(info.height*scale+.5))
  local small=original:resize(width,height,"box")
  original:close()
  local rgba=small:read{x=0,y=0,width=width,height=height}
  small:close()
  local hex=rgba:gsub(".",function(byte)return string.format("%02x",byte:byte())end)
  plum.config.project.set(CAPTURE_KEY,{version=1,width=width,height=height,hex=hex})
  print(string.format("Captured %dx%d pixels in this project's plugin settings. No file path or grant is stored.",width,height))
end
local function source_panel()
  local source=source_image()
  local image=plum.ui.image(source,{max_side=256,filter="nearest"})
  source:close()
  local panel=plum.ui.open_panel{title="Image Art Studio — Captured Source",width=480,height=420,content={id="root",kind="column",children={
    {id="about",kind="label",label="Captured pixels or Lua-authored default. Use Tools > Custom for MIDI Preview/Apply."},
    {id="image",kind="canvas",height=280,primitives={{kind="rect",x1=0,y1=0,x2=1,y2=1,fill=true,color={40,40,40,255}},{kind="image",image=image,x1=0,y1=0,x2=1,y2=1}}},
    {id="close",kind="button",label="Close source preview"}
  }}}
  while true do local event=panel:next_event();if not event or event.kind=="close" or event.id=="close" then panel:close();image:close();return end end
end
local function prepared(ctx)
  local p=ctx.params
  local first=math.min(ctx.keys-1,p.first)
  local height=math.min(p.height,ctx.keys-first)
  local original=source_image()
  local image=original:resize(p.width,height,"box")
  original:close()
  return image,first,height
end
local function palette(name)
  if name=="Authored RGB" then return {{255,0,0},{0,255,0},{0,0,255},{0,0,0},{255,255,255}} end
  if name=="Luma bands" then return {{0,0,0},{64,64,64},{128,128,128},{192,192,192},{255,255,255}} end
  return {{255,255,255}}
end
local function artwork(ctx)
  local p=ctx.params
  local image,first=prepared(ctx)
  local colors=palette(p.palette)
  local plan=image:note_plan{palette=colors,keys=ctx.keys,first_key=first,start_tick=p.start,
    tick_numerator=p.step,velocity=p.velocity,alpha_min=p.alpha,pixel_notes=p.pixels}
  image:close()
  local mapping
  local kind=ctx.track:kind()
  if p.route and (kind=="track" or kind=="subtrack")then
    -- Existing native colors remain authoritative. Never invent per-note RGBA.
    local subs=ctx.track:subchannels()
    mapping={};for i=1,#colors do mapping[i]=subs[(i-1)%#subs+1]end
  end
  -- Owned Clip content uses its captured native host's main Subchannel by default.
  local writer=ctx.tx:note_writer(ctx.track)
  local count=writer:append_plan(plan,mapping)
  writer:finish();plan:close()
  return string.format("Image artwork: %d native notes; existing Track/Subchannel colors.",count)
end
local function conditioned(ctx)
  local p=ctx.params
  local image,first,height=prepared(ctx)
  local options={kind="alpha"}
  if p.mask=="Luminance"then options={kind="luminance",preserve_alpha=true}
  elseif p.mask=="Brightness"then options={kind="brightness",preserve_alpha=true}
  elseif p.mask=="Green key"then options={kind="color",color={0,255,0},tolerance=.25,preserve_alpha=true}end
  local mask=image:mask(options);image:close()
  local sampler=mask:note_sampler{start_tick=p.start,end_tick=plum.ticks.add(p.start,plum.ticks.mul(p.width,p.step)),
    first_key=first,last_key=first+height-1,sample=p.coverage,threshold=p.threshold}
  mask:close()
  local edit={action="modify",changes={}}
  if p.operation=="Delete"then edit={action="delete"}
  elseif p.operation=="Select"then edit={action="select",mode="replace"}
  elseif p.operation=="Duplicate"then edit={action="duplicate",changes={shift=p.shift,transpose=p.transpose,outside="clamp"}}
  elseif p.operation=="Velocity"then edit.changes.velocity=p.velocity
  elseif p.operation=="Gate"then edit.changes.gate=p.gate
  elseif p.operation=="Transpose"then edit.changes={transpose=p.transpose,outside="clamp"}
  else edit.changes.shift=p.shift end
  local count=ctx.tx:mask_notes(ctx.notes,sampler,edit)
  sampler:close()
  return string.format("Image mask %s: %d matched source notes (%s coverage).",p.operation,count,p.coverage)
end
local function common()
  return {
    {key="start",kind="ticks",label="Song / Clip-local start",min=0,max=1000000000000,default=0},
    {key="first",kind="pitch",label="Lowest image key",min=0,max=255,default=48},
    {key="width",kind="int",label="Time columns",min=1,max=64,default=32},
    {key="height",kind="int",label="Pitch rows (clipped to key mode)",min=1,max=64,default=24},
    {key="step",kind="ticks",label="Ticks per image column",min=1,max=15360,default=120},
    {key="velocity",kind="int",label="Velocity",min=1,max=127,default=100}
  }
end
function register(plugin)
  plugin.command{id="choose_source",name="Image Art Studio: Choose Image...",run=choose_source}
  plugin.command{id="show_source",name="Image Art Studio: Show Captured Source",run=source_panel}
  plugin.command{id="reset_source",name="Image Art Studio: Reset to Authored Default",run=function()
    assert(plum.project.current(),"Open a project first.");plum.config.project.remove(CAPTURE_KEY)
  end}
  plugin.command{id="save_source",name="Image Art Studio: Save Capture Preset",run=function()
    local capture=plum.config.project.get(CAPTURE_KEY)
    assert(capture==nil or capture_valid(capture),"Invalid source capture.")
    plum.presets.save(PRESET,{version=1,capture=capture or false})
  end}
  plugin.command{id="load_source",name="Image Art Studio: Load Capture Preset",run=function()
    assert(plum.project.current(),"Open a project first.")
    local preset=assert(plum.presets.load(PRESET),"Save an Image Art Studio Capture preset first.")
    assert(type(preset)=="table" and preset.version==1 and (preset.capture==false or capture_valid(preset.capture)),"Invalid capture preset.")
    if preset.capture==false then plum.config.project.remove(CAPTURE_KEY)else plum.config.project.set(CAPTURE_KEY,preset.capture)end
  end}
  local art=common()
  art[#art+1]={key="palette",kind="choice",label="Author palette",choices={"Monochrome","Authored RGB","Luma bands"},default="Authored RGB"}
  art[#art+1]={key="alpha",kind="int",label="Minimum pixel alpha",min=0,max=255,default=128}
  art[#art+1]={key="pixels",kind="bool",label="One note per pixel instead of equal-color runs",default=false}
  art[#art+1]={key="route",kind="bool",label="Cycle existing native Subchannels (Track/Subtrack)",default=true}
  plugin.tool{id="artwork",name="Image Art Studio — Note Artwork",needs_selection=false,params=art,
    tooltip="Author-chosen palette/runs from captured pixels. Preview/Apply share native transactions; Clip-local ticks and native colors.",run=artwork}
  local mask=common()
  mask[#mask+1]={key="mask",kind="choice",label="Image interpretation",choices={"Alpha silhouette","Luminance","Brightness","Green key"},default="Alpha silhouette"}
  mask[#mask+1]={key="coverage",kind="choice",label="Note coverage",choices={"start","center","any","all","average"},default="start"}
  mask[#mask+1]={key="threshold",kind="int",label="Mask threshold",min=0,max=255,default=128}
  mask[#mask+1]={key="operation",kind="choice",label="Author operation",choices={"Velocity","Gate","Transpose","Shift","Delete","Duplicate","Select"},default="Velocity"}
  mask[#mask+1]={key="gate",kind="ticks",label="Gate for Gate operation",min=0,max=15360,default=96}
  mask[#mask+1]={key="transpose",kind="int",label="Transpose semitones",min=-48,max=48,default=12}
  mask[#mask+1]={key="shift",kind="int",label="Tick shift / duplicate offset",min=-15360,max=15360,default=120}
  plugin.tool{id="mask",name="Image Art Studio — Mask Editing",needs_selection=false,params=mask,
    tooltip="Uses the selected notes, or all notes if nothing is selected. Native indexed mask editing, Preview/Apply and Undo.",run=conditioned}
end
