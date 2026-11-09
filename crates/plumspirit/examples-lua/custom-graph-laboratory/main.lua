plum.require_api(0,2)
-- Every graph algorithm, domain mapping, gesture and musical interpretation below
-- is authored Lua. The host supplies only bounded UI/geometry/transactions.
local tau=2*math.pi
local function clamp(v,a,b)return math.max(a,math.min(b,v))end
local function num(v,d,a,b)
  return type(v)=="number" and v==v and v~=math.huge and v~=-math.huge and clamp(v,a,b) or d
end
local function defaults(mode)
  return {version=1,mode=mode or 1,bezier={{x=0,y=0},{x=.25,y=1},{x=.75,y=0},{x=1,y=1}},
    envelope={{x=0,y=-.5},{x=.5,y=.8},{x=1,y=-.2}},petals=5,phase=0,radius=1,
    count=64,seed=1,jitter=0,gate=.8,operation=1,interpolation=1,
    view={cx=mode==2 and 0 or .5,cy=mode==2 and 0 or .5,sx=mode==2 and 2.6 or 1.3,sy=mode==2 and 2.6 or 1.3}}
end
local function restore(raw,mode)
  local s=defaults(mode)
  if type(raw)~="table" or raw.version~=1 then return s end
  s.mode=math.floor(num(raw.mode,s.mode,1,3))
  for k,r in pairs{petals={1,12},phase={-tau,tau},radius={.1,2},count={2,512},seed={1,65535},jitter={0,1},gate={.01,1},operation={1,2},interpolation={1,2}}do s[k]=num(raw[k],s[k],r[1],r[2])end
  for _,k in ipairs{"petals","count","seed","operation","interpolation"}do s[k]=math.floor(s[k])end
  for _,k in ipairs{"bezier","envelope"}do
    if type(raw[k])=="table" and #raw[k]>=(k=="bezier" and 4 or 2) and #raw[k]<=(k=="bezier" and 4 or 32)then
      local points={}
      for _,p in ipairs(raw[k])do if type(p)=="table"then points[#points+1]={x=num(p.x,0,k=="envelope" and 0 or -2,k=="envelope" and 1 or 2),y=num(p.y,0,-2,2)}end end
      if #points==#raw[k]then s[k]=points end
    end
  end
  table.sort(s.envelope,function(a,b)return a.x<b.x end)
  if type(raw.view)=="table"then for _,k in ipairs{"cx","cy","sx","sy"}do s.view[k]=num(raw.view[k],s.view[k],k:sub(1,1)=="s" and .001 or -4,k:sub(1,1)=="s" and 8 or 4)end end
  return s
end
local function snapshot(s)
  local r={version=1,mode=s.mode,view={},bezier={},envelope={}}
  for _,k in ipairs{"petals","phase","radius","count","seed","jitter","gate","operation","interpolation"}do r[k]=s[k]end
  for k,v in pairs(s.view)do r.view[k]=v end
  for _,k in ipairs{"bezier","envelope"}do for i,p in ipairs(s[k])do r[k][i]={x=p.x,y=p.y}end end
  return r
end
local function evaluate(s,t)
  if s.mode==1 then
    local p=s.bezier;local u=1-t
    return u^3*p[1].x+3*u*u*t*p[2].x+3*u*t*t*p[3].x+t^3*p[4].x,
           u^3*p[1].y+3*u*u*t*p[2].y+3*u*t*t*p[3].y+t^3*p[4].y
  elseif s.mode==2 then
    local theta=t*tau;local r=s.radius*(.55+.45*math.cos(s.petals*theta+s.phase))
    return r*math.cos(theta),r*math.sin(theta)
  else
    local p=s.envelope
    if t<=p[1].x then return t,p[1].y end
    for i=2,#p do if t<=p[i].x then
      local d=p[i].x-p[i-1].x;local u=d>0 and (t-p[i-1].x)/d or 0
      return t,s.interpolation==2 and p[i-1].y or p[i-1].y+(p[i].y-p[i-1].y)*u
    end end
    return t,p[#p].y
  end
end
local function generate(s,keys)
  local notes={};local seed=s.seed
  for i=1,s.count do
    local t=(i-1)/(s.count-1);local x,y=evaluate(s,t)
    if s.mode==2 then x=(x+2)/4;y=(y+2)/4 elseif s.mode==3 then y=(y+1)/2 end
    seed=(seed*48271)%2147483647
    notes[i]={start=math.floor(clamp(x,0,1)*(s.count-1)*120+.5),gate=math.max(1,math.floor(120*s.gate+.5)),
      key=math.floor(clamp(y,0,1)*(keys-1)+.5),velocity=math.floor(clamp(32+t*95+(seed/2147483647-.5)*40*s.jitter,1,127)+.5)}
  end
  return notes
end
local function project_point(v,x,y)return .5+(x-v.cx)/v.sx,.5-(y-v.cy)/v.sy end
local function domain(v,x,y)return v.cx+(x-.5)*v.sx,v.cy+(.5-y)*v.sy end
local function laboratory(mode)
  local saved=plum.presets.load("Graph Laboratory")
  local s=restore(saved,mode);s.mode=mode
  if type(saved)=="table" and saved.mode~=mode then s.view=defaults(mode).view end
  local project=plum.project.current();local keys=project and project:key_range() or 128
  local selected=s.mode==3 and s.envelope[2] or s.bezier[2]
  local panel,preview,preview_model,gesture,selection_span,preview_total
  local status="Drag orange handles; middle-drag pans; wheel zooms. Preview before Apply."
  local maps={};local map_order={}
  local function spec()
    local primitives={};local v=s.view
    local function line(x1,y1,x2,y2,color,width)
      local a,b=project_point(v,x1,y1);local c,d=project_point(v,x2,y2)
      local q=plum.geometry.clip_segment({x_min=0,y_min=0,x_max=1,y_max=1},a,b,c,d)
      if q then primitives[#primitives+1]={kind="line",x1=q.x0,y1=q.y0,x2=q.x1,y2=q.y1,color=color,width=width or 1}end
    end
    local grid={65,70,90,160};local orange={255,180,65,255};local blue={90,185,255,255}
    for i=0,4 do
      local x=v.cx-v.sx/2+v.sx*i/4;local y=v.cy-v.sy/2+v.sy*i/4
      line(x,v.cy-v.sy/2,x,v.cy+v.sy/2,grid);line(v.cx-v.sx/2,y,v.cx+v.sx/2,y,grid)
      primitives[#primitives+1]={kind="text",x=i/4,y=.94,text=string.format("%.3f",x),color={160,170,190,255}}
      primitives[#primitives+1]={kind="text",x=.01,y=1-i/4,text=string.format("%.3f",y),color={160,170,190,255}}
    end
    line(0,v.cy-v.sy/2,0,v.cy+v.sy/2,{130,130,150,255});line(v.cx-v.sx/2,0,v.cx+v.sx/2,0,{130,130,150,255})
    local x,y=evaluate(s,0)
    for i=1,256 do local a,b=evaluate(s,i/256);line(x,y,a,b,blue,2);x,y=a,b end
    if s.mode~=2 then
      local points=s.mode==1 and s.bezier or s.envelope
      if s.mode==1 then for i=2,#points do line(points[i-1].x,points[i-1].y,points[i].x,points[i].y,{180,130,65,160})end end
      for _,p in ipairs(points)do local x,y=project_point(v,p.x,p.y);if x>=0 and x<=1 and y>=0 and y<=1 then primitives[#primitives+1]={kind="circle",x=x,y=y,radius=p==selected and 7 or 5,color=orange,fill=true}end end
    end
    if preview then
      for _,n in ipairs(preview)do
        local width=selection_span and math.max(1,plum.ticks.sub(selection_span.last,selection_span.first)) or s.count*120
        local x=selection_span and plum.ticks.sub(n.start,selection_span.first)/width or n.start/width;local y=1-n.key/(keys-1)
        primitives[#primitives+1]={kind="rect",x1=x,y1=clamp(y-.004,0,1),x2=math.min(1,x+n.gate/width),y2=clamp(y+.004,0,1),fill=true,color={100,math.floor(40+n.velocity*1.5),160,170}}
      end
    end
    local function number(id,label,value,min,max,step,enabled)return {id=id,kind="number",label=label,value=value,min=min,max=max,step=step or .01,enabled=enabled~=false}end
    return {title="Custom Graph Laboratory",width=760,height=700,content={id="root",kind="column",children={
      {id="mode",kind="choice",label="Graph",choices={"Editable Bézier","Polar rose","MIDI envelope"},value=s.mode},
      {id="view",kind="row",children={number("cx","X center",v.cx,-4,4),number("cy","Y center",v.cy,-4,4),number("sx","X span",v.sx,.001,8),number("sy","Y span",v.sy,.001,8)}},
      {id="graph",kind="canvas",height=320,primitives=primitives},
      {id="coordinates",kind="row",children={number("pointx","Point X",selected and selected.x or 0,-2,2,.001,s.mode~=2),number("pointy","Point Y",selected and selected.y or 0,-2,2,.001,s.mode~=2),number("petals","Petals",s.petals,1,12,1,s.mode==2),number("phase","Phase radians",s.phase,-tau,tau,.01,s.mode==2),number("radius","Radius",s.radius,.1,2,.01,s.mode==2)}},
      {id="interpolation",kind="choice",label="Envelope interpolation",choices={"Linear","Step"},value=s.interpolation,enabled=s.mode==3},
      {id="music",kind="row",children={number("count","Notes",s.count,2,512,1),number("seed","Seed",s.seed,1,65535,1),number("jitter","Velocity jitter",s.jitter,0,1),number("gate","Gate",s.gate,.01,1)}},
      {id="operation",kind="choice",label="Apply operation",choices={"Generate notes","Map selected velocity"},value=s.operation},
      {id="actions",kind="row",children={{id="preview",kind="button",label="Preview"},{id="apply",kind="button",label="Apply MIDI",enabled=preview~=nil and project~=nil},{id="save",kind="button",label="Save preset"},{id="load",kind="button",label="Load preset"},{id="reset",kind="button",label="Reset graph"},{id="done",kind="button",label="Close without applying"},{id="cancel",kind="button",label="Cancel job"}}},
      {id="status",kind="label",label=status},
    }}}
  end
  local function remember(revision)
    maps[revision]={cx=s.view.cx,cy=s.view.cy,sx=s.view.sx,sy=s.view.sy};map_order[#map_order+1]=revision
    if #map_order>8 then maps[table.remove(map_order,1)]=nil end
  end
  local function refresh()remember(panel:update(spec()))end
  panel=plum.ui.open_panel(spec());remember(1)
  while true do
    local e=panel:next_event();if not e then return end
    local changed=false
    if e.kind=="activate"then
      if e.id=="preview"then
        preview_model=snapshot(s);selection_span=nil
        if s.operation==1 then preview=generate(preview_model,keys);preview_total=#preview
        elseif project then
          local query=assert(project:track(1)):notes{selected=true};preview_total=query:count()
          assert(preview_total<=8192,"This demonstration limits selection mapping to8192notes; narrow the selection.")
          local a,b
          for batch in query:batches{size=4096,columns={"start"}}do if batch.count>0 then a=a or batch.start[1];b=batch.start[batch.count]end end
          preview={}
          if a then
            selection_span={first=a,last=b}
            for batch in query:batches{size=4096,columns={"start","gate","key"}}do
              for i=1,batch.count do
                if #preview<512 then
                  local t=b>a and plum.ticks.sub(batch.start[i],a)/plum.ticks.sub(b,a) or 0
                  local _,y=evaluate(preview_model,t)
                  preview[#preview+1]={start=batch.start[i],gate=batch.gate[i],key=batch.key[i],velocity=math.floor(clamp((y+2)/4*127,1,127)+.5)}
                end
              end
            end
          end
        else preview=nil;preview_total=0 end
        status=preview and string.format("Preview: %d of %d note cells; green intensity is velocity; project unchanged.",#preview,preview_total) or "Open a project for selection preview."
      elseif e.id=="apply" and preview and project then
        local tx=plum.transaction{label="Custom Graph Laboratory"};local target=assert(project:track(1))
        if preview_model.operation==1 then
          local writer=tx:note_writer(target);writer:append(preview);writer:finish()
        else
          local query=target:notes{selected=true};local span=selection_span
          if span then tx:modify_notes(query,function(n)
            local t=span.last>span.first and plum.ticks.sub(n.start,span.first)/plum.ticks.sub(span.last,span.first) or 0
            local _,y=evaluate(preview_model,t);return {velocity=math.floor(clamp((y+2)/4*127,1,127)+.5)}
          end)end
        end
        tx:commit();plum.config.set("last_graph",snapshot(s));panel:close();return
      elseif e.id=="save"then plum.presets.save("Graph Laboratory",snapshot(s));status="Preset saved: original graph coordinates and view."
      elseif e.id=="load"then s=restore(plum.presets.load("Graph Laboratory"),s.mode);selected=s.mode==3 and s.envelope[2] or s.bezier[2];changed=true
      elseif e.id=="reset"then s=defaults(s.mode);selected=s.mode==3 and s.envelope[2] or s.bezier[2];changed=true
      elseif e.id=="done"then panel:close();return
      elseif e.id=="cancel"then panel:cancel();return end
    elseif e.kind=="change"then
      if e.id=="mode"then s.mode=e.value;s.view=defaults(s.mode).view;selected=s.mode==3 and s.envelope[2] or s.bezier[2]
      elseif e.id=="cx" or e.id=="cy" or e.id=="sx" or e.id=="sy"then s.view[e.id]=e.value
      elseif e.id=="pointx" and selected then selected.x=clamp(e.value,s.mode==3 and 0 or -2,s.mode==3 and 1 or 2)
      elseif e.id=="pointy" and selected then selected.y=e.value
      elseif s[e.id]~=nil then s[e.id]=e.value end
      for _,k in ipairs{"petals","count","seed","operation","interpolation"}do s[k]=math.floor(s[k])end
      table.sort(s.envelope,function(a,b)return a.x<b.x end);changed=true
    elseif e.id=="graph" and e.pointer then
      local p=e.pointer;local v=maps[e.revision]
      if not v then gesture=nil;status="Stale view input discarded; start the gesture again."
      elseif e.kind=="scroll"then
        local amount=p.scroll_y*(p.scroll_unit=="lines" and .12 or p.scroll_unit=="pages" and 1 or .004)
        local factor=math.exp(clamp(-amount,-1,1));s.view.sx=clamp(s.view.sx*factor,.001,8);s.view.sy=clamp(s.view.sy*factor,.001,8);changed=true
      elseif e.kind=="pointer_down"then
        local x,y=domain(v,p.x,p.y)
        if p.buttons&4~=0 then gesture={pan=true,x=p.x,y=p.y,cx=v.cx,cy=v.cy,sx=v.sx,sy=v.sy}
        elseif s.mode==2 then s.phase=math.atan(y,x);changed=true
        else
          local points=s.mode==1 and s.bezier or s.envelope;local near,distance
          for i,point in ipairs(points)do local a,b=project_point(v,point.x,point.y);local d=((a-p.x)*p.width)^2+((b-p.y)*p.height)^2;if not distance or d<distance then near,distance=i,d end end
          if p.buttons&2~=0 and s.mode==3 and distance<144 and #points>2 then table.remove(points,near);selected=points[1];changed=true
          elseif distance<144 then selected=points[near];gesture={point=selected};changed=true
          elseif s.mode==3 and #points<32 then selected={x=clamp(x,0,1),y=clamp(y,-2,2)};points[#points+1]=selected;gesture={point=selected};table.sort(points,function(a,b)return a.x<b.x end);changed=true end
        end
      elseif e.kind=="pointer_move" and gesture and p.buttons~=0 then
        if gesture.pan then s.view.cx=clamp(gesture.cx-(p.x-gesture.x)*gesture.sx,-4,4);s.view.cy=clamp(gesture.cy+(p.y-gesture.y)*gesture.sy,-4,4)
        else local x,y=domain(v,p.x,p.y);gesture.point.x=clamp(x,s.mode==3 and 0 or -2,s.mode==3 and 1 or 2);gesture.point.y=clamp(y,-2,2);table.sort(s.envelope,function(a,b)return a.x<b.x end)end;changed=true
      elseif e.kind=="pointer_up" or e.kind=="pointer_cancel"then gesture=nil end
    end
    if changed then preview=nil;preview_model=nil;selection_span=nil;status="Graph changed; Preview again before Apply."end
    if e.kind~="opened" and e.kind~="resize" and not(e.kind=="pointer_move" and not gesture and not changed)then refresh()end
  end
end
function register(p)
  p.command{id="bezier",name="Custom Graph Laboratory: Bézier",run=function()laboratory(1)end}
  p.command{id="polar",name="Custom Graph Laboratory: Polar",run=function()laboratory(2)end}
  p.command{id="midi",name="Custom Graph Laboratory: MIDI Envelope",run=function()laboratory(3)end}
end
