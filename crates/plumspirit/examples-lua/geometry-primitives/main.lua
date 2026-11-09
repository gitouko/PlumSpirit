plum.require_api(0,2)
-- All creative decisions are authored here; the host supplies bounded math only.
local function emit(sampled,label)
  local project=assert(plum.project.current(),"Open a project first.")
  local track=assert(project:track(1),"The project needs a Track.")
  local mapping=plum.geometry.mapping{scale_x=1,scale_y=-1,translate_y=1}
  local notes={}
  for i=1,sampled.count do
    local x,y=plum.geometry.map_point(mapping,sampled.x[i],sampled.y[i])
    notes[i]={start=(i-1)*120,gate=96,key=math.floor(48+y*24+0.5),velocity=math.floor(60+x*40+0.5)}
  end
  local tx=plum.transaction{label=label}
  tx:insert_notes(track,notes)
  tx:commit()
end
function register(p)
  p.command{id="draw-path",name="Generate Author Path",run=function()
    local path={x={0,1,0.5,0},y={0,0,1,0}}
    emit(plum.geometry.resample_polyline(path,32),"Author Geometry Path")
  end}
  p.command{id="draw-cubic",name="Generate Author Bezier",run=function()
    local control={x={0,0.2,0.8,1},y={0,1,0,1}}
    emit(plum.geometry.sample_cubic(control,32),"Author Bezier Notes")
  end}
  p.command{id="draw-mixed-path",name="Generate Author Mixed Path",run=function()
    local path={start={x=0,y=0},closed=true,segments={
      {kind="line",x=0.25,y=0.3},
      {kind="cubic",x1=0.5,y1=1,x2=0.7,y2=0,x=0.8,y=0.8},
      {kind="line",x=1,y=1},
    }}
    local sampled=plum.geometry.sample_path(path,{curve_steps=16})
    emit(plum.geometry.resample_polyline(sampled,32),"Author Mixed Path Notes")
  end}
  p.command{id="draw-adaptive-mask",name="Generate Author Adaptive Mask",run=function()
    local g=plum.geometry
    local control={x={0,0.2,0.8,1},y={0,1,0,1}}
    local sampled=g.resample_polyline(g.flatten_cubic(control,{tolerance=0.001}),32)
    local mask={x={0,1,0},y={0,0,1}}
    local guide={x={0,1},y={0,1}}
    for i=1,sampled.count do
      local x,y=sampled.x[i],sampled.y[i]
      -- Author's mask chooses pitch reflection; distance controls velocity.
      if g.polygon_classify(mask,x,y)=="outside" then sampled.y[i]=1-y end
      sampled.x[i]=1-math.min(1,g.nearest_polyline(guide,x,y).distance)
    end
    emit(sampled,"Author Adaptive Mask Notes")
  end}
  p.command{id="draw-adaptive-path",name="Generate Author Adaptive Path",run=function()
    local path={start={x=0,y=0},closed=true,segments={
      {kind="line",x=0.2,y=0.3},
      {kind="cubic",x1=0.2,y1=1,x2=0.7,y2=0,x=0.8,y=0.8},
      {kind="line",x=1,y=1},
    }}
    local g=plum.geometry
    local sampled=g.flatten_path(path,{tolerance=0.001,max_points=1024})
    emit(g.resample_polyline(sampled,32),"Author Adaptive Path Notes")
  end}
  p.command{id="draw-raster-cells",name="Generate Author Raster Cells",run=function()
    local project=assert(plum.project.current(),"Open a project first.")
    local track=assert(project:track(1),"The project needs a Track.")
    local polygon={x={0,11,11,0},y={0,0,3,3}}
    local spans=plum.geometry.rasterize_polygon(polygon,{columns=11,rows=3})
    local tx=plum.transaction{label="Author Raster Cell Notes"}
    local writer=tx:note_writer(track)
    for i=1,spans.count do
      local notes={start={},gate={},key={},velocity={}}
      for column=spans.first[i],spans.last[i]-1 do
        local j=#notes.start+1
        notes.start[j]=plum.ticks.add(0,column*120)
        notes.gate[j]=96
        notes.key[j]=project:key_range()-3+spans.row[i]
        notes.velocity[j]=72+spans.row[i]*16
      end
      writer:append(notes)
    end
    writer:finish();tx:commit()
  end}
end
