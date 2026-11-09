plum.require_api(0,2)
-- Every visual/mapping below is authored Lua; the host supplies bounded resources.
local function run()
  local rows={}
  for y=0,63 do
    local row={}
    for x=0,63 do
      local odd=(math.floor(x/8)+math.floor(y/8))%2==1
      row[#row+1]=string.char(odd and 240 or 30,math.floor(x*255/63),math.floor(y*255/63),32+math.floor(x*223/63))
    end
    rows[#rows+1]=table.concat(row)
  end
  local src=plum.images.from_rgba(64,64,table.concat(rows))
  local image=plum.ui.image(src,{max_side=512,filter="linear"});src:close()
  local opacity,crop,x,y=255,0,.5,.5
  local reads=0
  local status="Authored checker/alpha gradient. Drag a marker; choose a file to replace the texture."
  local function spec()
    local primitives={}
    for row=0,7 do for col=0,7 do
      local c=(row+col)%2==0 and 45 or 90
      primitives[#primitives+1]={kind="rect",x1=col/8,y1=row/8,x2=(col+1)/8,y2=(row+1)/8,fill=true,color={c,c,c,255}}
    end end
    primitives[#primitives+1]={kind="image",image=image,x1=0,y1=0,x2=1,y2=1,u1=crop,v1=crop,u2=1-crop,v2=1-crop,color={255,255,255,opacity}}
    primitives[#primitives+1]={kind="line",x1=0,y1=.5,x2=1,y2=.5,color={255,220,70,190}}
    primitives[#primitives+1]={kind="line",x1=.5,y1=0,x2=.5,y2=1,color={255,220,70,190}}
    primitives[#primitives+1]={kind="circle",x=x,y=y,radius=7,fill=true,color={255,70,110,255}}
    local info=image:info()
    return {title="UI Image Laboratory",width=690,height=650,content={id="root",kind="column",children={
      {id="source",kind="label",label=string.format("Source %d x %d; UI %d x %d; shared native texture",info.source_width,info.source_height,info.width,info.height)},
      {id="open",kind="button",label="Choose image...",enabled=reads<8},
      {id="opacity",kind="slider",label="Opacity",min=0,max=255,step=1,value=opacity},
      {id="crop",kind="slider",label="UV crop",min=0,max=.45,step=.01,value=crop},
      {id="canvas",kind="canvas",height=320,primitives=primitives},
      {id="status",kind="label",label=status},
      {id="close",kind="button",label="Close laboratory"},
    }}}
  end
  local panel=plum.ui.open_panel(spec())
  local held,last_time=false,-1
  while true do
    local e=panel:next_event();if not e then return end
    local update=false
    if e.kind=="activate" and e.id=="open" and reads<8 then
      reads=reads+1
      local f=plum.files.open{filters={{name="Images",extensions={"png","jpg","jpeg","bmp","gif","webp","tif","tiff"}}}}
      if f then
        local replacement=plum.images.load(f);f:close()
        local fresh=plum.ui.image(replacement,{max_side=512,filter="linear"});replacement:close()
        image:close();image=fresh
        status="User-selected image; file and source Image closed after conversion."
      else status="File selection canceled; original image retained."end
      update=true
    elseif e.kind=="change" and e.id=="opacity" then opacity=math.floor(e.value+.5);update=true
    elseif e.kind=="change" and e.id=="crop" then crop=e.value;update=true
    elseif e.id=="canvas" then
      if e.kind=="pointer_cancel" then held=false end
      if e.kind=="pointer_down" then held=true end
      if held and e.pointer and (e.kind=="pointer_down" or e.kind=="pointer_move" or e.kind=="pointer_up") then
        x=math.max(0,math.min(1,e.pointer.x));y=math.max(0,math.min(1,e.pointer.y))
        status=string.format("Author marker %.6f, %.6f",x,y)
        if e.kind=="pointer_down" or e.kind=="pointer_up" or e.pointer.time-last_time>=.05 then last_time=e.pointer.time;update=true end
      end
      if e.kind=="pointer_up" then held=false end
    elseif e.kind=="activate" and e.id=="close" then panel:close();image:close();return end
    if update then panel:update(spec())end
  end
end
function register(plugin)
  plugin.command{id="open",name="Open UI Image Laboratory",run=run}
end
