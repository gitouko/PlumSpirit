plum.require_api(0,2)
-- All sampling, typography, layout, raster union and MIDI choices are authored Lua.
local function raster_glyph(font,id,origin_x,origin_y,scale,filled,grid,collect,cache)
  local outline=cache and cache[id]or font:glyph_outline(id)
  if cache then cache[id]=outline end
  if outline.count==0 then return end -- spaces still retain their shaped advances
  local b=outline.control_bounds
  if origin_x+b.x_max*scale < -1 or origin_x+b.x_min*scale > grid.columns+1
    or origin_y-b.y_min*scale < -1 or origin_y-b.y_max*scale > grid.rows+1 then return end
  local g=plum.geometry
  local contours={}
  local mapped=g.map_points(g.mapping{scale_x=scale,scale_y=-scale,
    translate_x=origin_x,translate_y=origin_y},
    {count=outline.count,x=outline.x,y=outline.y})
  for c=1,outline.contour_count do
    local first,last=outline.first[c],outline.last[c]
    local points={x={mapped.x[first]},y={mapped.y[first]}}
    local x,y=mapped.x[first],mapped.y[first]
    for i=first+1,last-1,3 do
      local sampled=g.flatten_cubic({x={x,mapped.x[i],mapped.x[i+1],mapped.x[i+2]},
        y={y,mapped.y[i],mapped.y[i+1],mapped.y[i+2]}},
        {tolerance=0.02,max_points=256,max_depth=16})
      assert(#points.x+sampled.count-1<=4096,"Author contour sampling limit reached.")
      for j=2,sampled.count do points.x[#points.x+1]=sampled.x[j];points.y[#points.y+1]=sampled.y[j]end
      x,y=mapped.x[i+2],mapped.y[i+2]
    end
    contours[#contours+1]=points
    if not filled then collect(g.rasterize_polyline(points,grid,{closed=true}))end
  end
  -- All contours are filled together so counters such as O remain holes.
  if filled and #contours>0 then collect(g.rasterize_region(contours,grid,{rule="non_zero"}))end
end
local function collector(columns)
  local cells={}
  return cells,function(spans)
    for i=1,spans.count do for column=spans.first[i],spans.last[i]-1 do cells[spans.row[i]*columns+column]=true end end
  end
end
local function emit(tx,target,cells,grid,start,step,gate,velocity)
  local writer=tx:note_writer(target)
  local batch={start={},gate={},key={},velocity={}}
  local count=0
  for row=0,grid.rows-1 do for column=0,grid.columns-1 do
    if cells[row*grid.columns+column]then
      local i=#batch.start+1
      batch.start[i]=plum.ticks.add(start,plum.ticks.mul(column,step));batch.gate[i]=gate
      batch.key[i]=grid.rows-1-row;batch.velocity[i]=velocity;count=count+1
      if i==4096 then writer:append(batch);batch={start={},gate={},key={},velocity={}}end
    end
  end end
  if #batch.start>0 then writer:append(batch)end
  writer:finish();return count
end
local function artwork(kind,text,filled,shaped)
  local project=assert(plum.project.current(),"Open a project first.")
  local track=assert(project:track(1),"A Track is required.")
  local font=plum.text.builtin_font(kind)
  local run
  if shaped then
    run=font:shape_run(text,{direction="ltr"})
    assert(run.missing_count==0,"The chosen font cannot render this text; original input was preserved.")
  else
    run={count=1,glyph_id={assert(font:glyph_id(text),"This font does not cover the requested scalar.")},x={0},y={0}}
  end
  local grid={columns=shaped and 256 or 64,rows=project:key_range()}
  local cells,collect=collector(grid.columns)
  for glyph=1,run.count do
    raster_glyph(font,run.glyph_id[glyph],1+run.x[glyph]*32,64-run.y[glyph]*32,32,filled,grid,collect)
  end
  font:close()
  local tx=plum.transaction{label="Author Glyph Artwork: "..text}
  emit(tx,track,cells,grid,0,120,96,96);tx:commit()
end
-- An explicit author policy for complex emoji in the new fallback modes.
-- Cmap component coverage alone must not draw an uncomposed family/keycap as if
-- it were the requested presentation. Ordinary combining-script text is unaffected.
local function require_composed_emoji(text,run)
  for cluster,first,last in plum.text.graphemes(text)do
    local sequence,joiner,pictograph=false,false,false
    for _,c in utf8.codes(cluster)do
      joiner=joiner or c==0x200d
      pictograph=pictograph or(c>=0x1f000 and c<=0x1faff)or(c>=0x2600 and c<=0x27ff)
      sequence=sequence or c==0x20e3 or(c>=0x1f3fb and c<=0x1f3ff)
        or(c>=0x1f1e6 and c<=0x1f1ff)
    end
    if sequence or(joiner and pictograph)then
      local count=0
      for i=1,run.count do if run.first[i]<last and run.last[i]>first then
        assert(run.first[i]==first and run.last[i]==last,"Author emoji cluster mapping is unsupported; original text is unchanged.")
        count=count+1
      end end
      assert(count==1,"The bundled fallback cannot compose this emoji sequence; original text is unchanged.")
    end
  end
end
local function composition(ctx)
  local p=ctx.params
  assert(utf8.len(p.text)<=128,"Text Art Studio allows128Unicode scalars.")
  local fallback=p.font=="Proportional + Emoji"or p.font=="Monospace + Emoji"
  local family=(p.font=="Monospace"or p.font=="Monospace + Emoji")and"monospace"or p.font=="Emoji"and"emoji"or"proportional"
  local font=plum.text.builtin_font(family)
  local fonts={font};if fallback then fonts[2]=plum.text.builtin_font("emoji")end
  local analysis=plum.text.bidi_runs(p.text,{direction=p.direction,max_lines=8,max_runs=128})
  local grid={columns=p.columns,rows=ctx.keys}
  local cells,collect=collector(grid.columns)
  local glyph_count=0
  local cache={{},{}} -- per-font invocation-local outlines; glyph IDs are face-local
  for line_index,line in ipairs(analysis.lines)do
    local runs={};local width=0
    for i=line.first_run,line.last_run-1 do
      local part=p.text:sub(analysis.first[i],analysis.last[i]-1)
      local scripts=plum.text.script_runs(part,{max_runs=128})
      for visual=1,scripts.count do
        local j=analysis.direction[i]=="rtl"and scripts.count-visual+1 or visual
        local piece=part:sub(scripts.first[j],scripts.last[j]-1)
        local coverage=fallback and plum.text.font_coverage_runs(piece,fonts,{max_runs=128})
          or{count=1,first={1},last={#piece+1},font={1},status={"covered"}}
        for font_visual=1,coverage.count do
          local k=analysis.direction[i]=="rtl"and coverage.count-font_visual+1 or font_visual
          assert(coverage.status[k]=="covered","The explicit font stack has missing glyphs for a complete grapheme; original text is unchanged.")
          local text=piece:sub(coverage.first[k],coverage.last[k]-1)
          local face=coverage.font[k]
          local run=fonts[face]:shape_run(text,{direction=analysis.direction[i],
            script=scripts.resolved[j]and scripts.script[j]or nil})
          assert(run.missing_count==0,"The chosen font has missing glyphs; original text is unchanged. Choose another supported font or text.")
          if fallback then require_composed_emoji(text,run)end
          glyph_count=glyph_count+run.count;assert(glyph_count<=256,"Author shaped-glyph limit reached.")
          runs[#runs+1]={run=run,pen=width,font=face};width=width+run.advance_x
        end
      end
    end
    local origin=1
    if p.alignment=="Center"then origin=(grid.columns-width*p.scale)/2
    elseif p.alignment=="Right"then origin=grid.columns-1-width*p.scale end
    local baseline=p.baseline+(line_index-1)*p.leading*p.scale
    for _,item in ipairs(runs)do local run=item.run
      for glyph=1,run.count do
        raster_glyph(fonts[item.font],run.glyph_id[glyph],origin+(item.pen+run.x[glyph])*p.scale,
          baseline-run.y[glyph]*p.scale,p.scale,p.filled,grid,collect,cache[item.font])
      end
    end
  end
  for _,face in ipairs(fonts)do face:close()end
  local count=emit(ctx.tx,ctx.track,cells,grid,p.start,p.step,p.gate,p.velocity)
  return string.format("Author composition: %d hard lines, %d shaped glyphs, %d clipped note cells.",analysis.line_count,glyph_count,count)
end
function register(p)
  p.command{id="outline-proportional",name="Generate Proportional Glyph Outline",run=function()artwork("proportional","O")end}
  p.command{id="outline-monospace",name="Generate Monospace Glyph Outline",run=function()artwork("monospace","O")end}
  p.command{id="outline-emoji",name="Generate Emoji Glyph Outline",run=function()artwork("emoji","😀")end}
  p.command{id="fill-proportional",name="Generate Filled Proportional Glyph",run=function()artwork("proportional","O",true)end}
  p.command{id="fill-monospace",name="Generate Filled Monospace Glyph",run=function()artwork("monospace","O",true)end}
  p.command{id="fill-emoji",name="Generate Filled Emoji Glyph",run=function()artwork("emoji","😀",true)end}
  p.command{id="shape-proportional",name="Generate Shaped Proportional Text",run=function()artwork("proportional","office é",true,true)end}
  p.command{id="shape-monospace",name="Generate Shaped Monospace Text",run=function()artwork("monospace","office é",true,true)end}
  p.command{id="shape-emoji",name="Generate Shaped Emoji Text",run=function()artwork("emoji","😀🔥",true,true)end}
  p.command{id="color-laboratory",name="Open Color Glyph Laboratory",run=function()
    plum.plugin.include("color-laboratory.lua").panel()
  end}
  p.tool{id="composition",name="Text Art Studio — Multiline Composition",needs_selection=false,
    tooltip="Author-defined glyph sampling, bidi runs, alignment and baselines. Native Preview/Apply/Undo and target-local ticks; clipping uses the actual key mode.",
    params={
      {key="text",kind="text",multiline=true,max_len=128,label="Text (128 Unicode scalars)",default="office é\nTwo lines"},
      {key="font",kind="choice",label="Bundled face",choices={"Proportional","Monospace","Emoji","Proportional + Emoji","Monospace + Emoji"},default="Proportional"},
      {key="direction",kind="choice",label="Base direction",choices={"auto","ltr","rtl"},default="auto"},
      {key="alignment",kind="choice",label="Author line alignment",choices={"Left","Center","Right"},default="Left"},
      {key="scale",kind="int",label="Note cells per em",min=4,max=64,default=24},
      {key="leading",kind="float",label="Baseline spacing (em)",min=.75,max=3,default=1.5},
      {key="baseline",kind="int",label="First baseline from top row",min=0,max=255,default=28},
      {key="columns",kind="int",label="Clipped time columns",min=16,max=512,default=256},
      {key="filled",kind="bool",label="Filled contours instead of outlines",default=true},
      {key="start",kind="ticks",label="Song / Clip-local start",min=0,max=1000000000000,default=0},
      {key="step",kind="ticks",label="Ticks per column",min=1,max=15360,default=120},
      {key="gate",kind="ticks",label="Gate",min=0,max=15360,default=96},
      {key="velocity",kind="int",label="Velocity",min=1,max=127,default=96}
    },run=composition}
  p.tool{id="color-artwork",name="Text Art Studio — Color Glyph Artwork",needs_selection=false,
    tooltip="Author maps fixed captured glyph pixels to existing native Subchannel colors. No font is reopened during Preview/Apply.",
    params={
      {key="width",kind="int",label="Time columns",min=1,max=64,default=32},
      {key="height",kind="int",label="Pitch rows",min=1,max=64,default=32},
      {key="first",kind="int",label="Lowest pitch (must fit actual key mode)",min=0,max=255,default=24},
      {key="start",kind="ticks",label="Song / Clip-local start",min=0,max=9223372036854775807,default=0},
      {key="step",kind="ticks",label="Ticks per pixel",min=1,max=15360,default=120},
      {key="velocity",kind="int",label="Velocity and native palette shade",min=1,max=127,default=100},
      {key="alpha",kind="int",label="Minimum alpha",min=1,max=255,default=1},
      {key="pixels",kind="bool",label="One note per pixel instead of equal-color runs",default=false},
      {key="colors",kind="int",label="First existing Subchannel colors",min=1,max=64,default=16}
    },run=function(ctx)return plum.plugin.include("color-laboratory.lua").artwork(ctx)end}

end
