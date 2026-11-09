plum.require_api(0,2)
-- The plugin authors every artistic algorithm. The host supplies capture,
-- bounded general geometry, note transactions, Preview and native Undo.
local g=plum.geometry
local function clamp(v,a,b)return math.max(a,math.min(b,v))end
local function source(ctx,limit)
  local info=ctx.stroke:info()
  assert(not info.at_sample_limit,"Stroke reached the native sample limit; draw a shorter gesture.")
  local step=ctx.params.step
  local base=info.min_tick//step*step
  local columns=(info.max_tick-base)//step+1
  assert(columns<=4096,"This demonstration limits a gesture to4096drawing columns. Increase Step or shorten the stroke.")
  local path={x={},y={}}
  local stride=math.max(1,math.ceil(info.count/(limit-1)))
  for first=1,info.count,1024 do
    local batch=ctx.stroke:batch(first,math.min(1024,info.count-first+1))
    for j,p in ipairs(batch)do
      local index=first+j-1
      if (index-1)%stride==0 or index==info.count then
        path.x[#path.x+1]=(p.tick-base)/step
        path.y[#path.y+1]=p.pitch
      end
    end
  end
  if #path.x==1 then path.x[2]=path.x[1];path.y[2]=path.y[1]end
  return info,path,base,columns
end
local function emitter(ctx,base,columns)
  local writer=ctx.tx:note_writer(ctx.track)
  local seen,count={},0
  local batch={start={},gate={},key={},velocity={},subchannel={}}
  local function flush()
    if #batch.start>0 then writer:append(batch);batch={start={},gate={},key={},velocity={},subchannel={}}end
  end
  local function cell(x,y,velocity)
    x=math.floor(x);y=math.floor(y)
    if x<0 or x>=columns or y<0 or y>=ctx.keys then return end
    local id=y*columns+x
    if seen[id]then return end
    count=count+1;assert(count<=8192,"This demonstration limits a gesture to8192notes; shorten it or increase Step.")
    seen[id]=true
    local j=#batch.start+1
    batch.start[j]=plum.ticks.add(base,plum.ticks.mul(x,ctx.params.step))
    batch.gate[j]=ctx.params.gate;batch.key[j]=y
    batch.velocity[j]=clamp(math.floor(velocity or ctx.params.velocity),1,127)
    batch.subchannel[j]=ctx.params.sub
    if j==256 then flush()end
  end
  return cell,function()flush();writer:finish()end
end
local function raster(ctx,path,columns,filled)
  local grid={columns=columns,rows=ctx.keys,max_spans=8192}
  return filled and g.rasterize_polygon(path,grid) or g.rasterize_polyline(path,grid)
end
local function paint_spans(ctx,spans,cell,radius)
  assert(spans.cell_count*(2*radius+1)<=8192,"Brush footprint exceeds this demonstration's8192note budget.")
  for i=1,spans.count do
    for x=spans.first[i],spans.last[i]-1 do
      for dy=-radius,radius do cell(x,spans.row[i]+dy)end
    end
  end
end
local function freehand(ctx)
  local _,path,base,columns=source(ctx,2048)
  local cell,finish=emitter(ctx,base,columns)
  paint_spans(ctx,raster(ctx,path,columns,false),cell,ctx.params.size-1)
  finish()
end
local function rectangle(ctx)
  local info,_,base,columns=source(ctx,2)
  local a,b=ctx.stroke:point(1),ctx.stroke:point(info.count)
  local x0=math.floor((math.min(a.tick,b.tick)-base)/ctx.params.step)
  local x1=math.floor((math.max(a.tick,b.tick)-base)/ctx.params.step)+1
  local y0=math.floor(math.min(a.pitch,b.pitch));local y1=math.floor(math.max(a.pitch,b.pitch))+1
  local shape={x={x0,x1,x1,x0,x0},y={y0,y0,y1,y1,y0}}
  local cell,finish=emitter(ctx,base,columns)
  paint_spans(ctx,raster(ctx,shape,columns,ctx.params.filled),cell,0)
  finish()
end
local function random(seed)
  -- Explicit reproducible32-bit LCG, independent of wall clock or math.random.
  local state=seed%4294967296
  return function()state=(1664525*state+1013904223)%4294967296;return state/4294967296 end
end
local function spray(ctx)
  local _,path,base,columns=source(ctx,512)
  local cell,finish=emitter(ctx,base,columns)
  local rng=random(ctx.seed)
  for i=1,#path.x do
    for _=1,ctx.params.density do
      -- Author-defined rectangular footprint and musical velocity falloff.
      local dx=(rng()*2-1)*ctx.params.size;local dy=(rng()*2-1)*ctx.params.size
      cell(path.x[i]+dx,path.y[i]+dy,ctx.params.velocity*(1-.35*math.abs(dy)/ctx.params.size))
    end
  end
  finish()
end
local function eraser(ctx)
  local _,path,base,columns=source(ctx,512)
  local end_tick=plum.ticks.add(base,plum.ticks.mul(columns,ctx.params.step))
  -- A native indexed start-range query avoids scanning the whole song. The
  -- deliberate candidate cap precedes Lua predicates and all musical writes.
  local query=ctx.track:notes{start_tick=base,end_tick=end_tick}
  assert(query:count()<=8192,"More than8192note onsets in this region; narrow the stroke.")
  ctx.tx:modify_notes(query,function(note)
    local x=(note.start-base)/ctx.params.step;local y=note.key+.5
    if g.nearest_polyline(path,x,y).distance<=ctx.params.size then return false end
  end)
end
local function wave(ctx)
  local _,path,base,columns=source(ctx,512)
  local sampled=g.resample_polyline(path,256)
  for i=1,sampled.count do
    local t=(i-1)/(sampled.count-1)
    sampled.y[i]=sampled.y[i]+ctx.params.size*math.sin(2*math.pi*ctx.params.cycles*t)
  end
  local cell,finish=emitter(ctx,base,columns)
  paint_spans(ctx,raster(ctx,sampled,columns,false),cell,0)
  finish()
end
local function params()
  return {
    {key="step",kind="ticks",label="Drawing step",min=1,max=15360,default=120},
    {key="gate",kind="ticks",label="Gate",min=1,max=15360,default=96},
    {key="velocity",kind="int",label="Velocity",min=1,max=127,default=100},
    {key="size",kind="int",label="Brush size / wave amplitude",min=1,max=8,default=2},
    {key="density",kind="int",label="Spray samples per point",min=1,max=16,default=4},
    {key="cycles",kind="float",label="Wave cycles",min=.25,max=16,default=3},
    {key="filled",kind="bool",label="Fill rectangle",default=true},
    {key="sub",kind="int",label="Existing Subchannel ID",min=0,max=255,default=0,
      help="Uses an existing ID on the edited Track; native Track/Subchannel colors remain authoritative."},
    {key="seed",kind="seed",label="Deterministic spray seed",default=1},
  }
end
function register(plugin)
  for _,tool in ipairs{
    {"freehand","Studio Freehand Brush",freehand},
    {"rectangle","Studio Rectangle Shape",rectangle},
    {"spray","Studio Deterministic Spray",spray},
    {"eraser","Studio Note-Onset Eraser",eraser},
    {"wave","Studio Mathematical Wave",wave},
  }do
    plugin.tool{id=tool[1],name=tool[2],tooltip="Draw in the Piano Roll. Inspect Preview before Apply. Native Undo restores each applied gesture.",
      gesture="freehand",needs_selection=false,params=params(),run=tool[3]}
  end
end
