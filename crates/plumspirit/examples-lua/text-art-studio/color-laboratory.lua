-- Author-owned color-font preview. The host supplies bounded public primitives.
local M={}
local KEY="color_glyph_inputs"
local PRESET="Color Glyph Laboratory Inputs"
local SIDE=64
local CAPTURE="color_glyph_capture"
local CAPTURE_PRESET="Color Glyph Laboratory Pixels"
function M.capture(image,source)
  local inputs=M.inputs{version=1,family=source.family,style=source.style,text=source.text,palette=source.palette}
  local info=image:info();assert(info.width==SIDE and info.height==SIDE,"Capture needs the rendered64square glyph.")
  local rgba=image:read{x=0,y=0,width=SIDE,height=SIDE}
  return {version=1,width=SIDE,height=SIDE,source=inputs,
    hex=rgba:gsub(".",function(byte)return string.format("%02x",byte:byte())end)}
end
function M.validate_capture(v)
  assert(type(v)=="table"and v.version==1 and v.width==SIDE and v.height==SIDE,
    "Unsupported color-glyph capture; Preview and Capture for MIDI first.")
  assert(type(v.hex)=="string"and #v.hex==SIDE*SIDE*8 and not v.hex:find("[^0-9a-fA-F]"),"Invalid bounded glyph pixels.")
  return {version=1,width=SIDE,height=SIDE,hex=v.hex,source=M.inputs(v.source)}
end
function M.artwork(ctx)
  local capture=M.validate_capture(assert(plum.config.project.get(CAPTURE),"Open Color Glyph Laboratory, Preview and Capture for MIDI first."))
  local p=ctx.params
  assert(p.first<ctx.keys,"First pitch is outside this project's key mode.")
  local height=math.min(p.height,ctx.keys-p.first)
  local bytes=capture.hex:gsub("..",function(pair)return string.char(tonumber(pair,16))end)
  local original=plum.images.from_rgba(SIDE,SIDE,bytes)
  local image=original:resize(p.width,height,"box");original:close()
  local subs=ctx.owner_track:subchannels();local colors,mapping={},{}
  for i=1,math.min(#subs,p.colors)do
    local c=subs[i]:note_color(p.velocity);colors[i]={c.r,c.g,c.b};mapping[i]=subs[i]
  end
  local plan=image:note_plan{palette=colors,keys=ctx.keys,first_key=p.first,start_tick=p.start,
    tick_numerator=p.step,velocity=p.velocity,alpha_min=p.alpha,pixel_notes=p.pixels}
  image:close()
  local writer=ctx.tx:note_writer(ctx.track);local count=writer:append_plan(plan,mapping)
  writer:finish();plan:close()
  return string.format("Captured %s: %d native notes using %d existing Subchannel colors.",capture.source.text,count,#mapping)
end
function M.inputs(v)
  assert(type(v)=="table"and v.version==1,"Unsupported color-glyph input version.")
  assert(type(v.family)=="string"and #v.family>0 and #v.family<=128 and utf8.len(v.family),"Font family needs1–128UTF-8bytes.")
  assert(type(v.style)=="string"and #v.style<=128 and utf8.len(v.style),"Style needs at most128UTF-8bytes.")
  assert(type(v.text)=="string"and #v.text<=256,"Glyph input needs at most256UTF-8bytes.")
  local count=utf8.len(v.text);assert(count and count>0 and count<=64,"Glyph input needs1–64Unicode scalars.")
  local clusters=0;for _ in plum.text.graphemes(v.text)do clusters=clusters+1 end
  assert(clusters==1,"This laboratory previews exactly one original grapheme; text is unchanged.")
  assert(math.type(v.palette)=="integer"and v.palette>=0 and v.palette<=65535,"Palette must be an integer0–65535.")
  return {version=1,family=v.family,style=v.style,text=v.text,palette=v.palette}
end
local function defaults()return{version=1,family="Segoe UI Emoji",style="",text="😀",palette=0}end
local function contour_spans(o,matrix)
  local g=plum.geometry;local contours={};local total=0
  if o.count==0 then return {row={},first={},last={}}end
  local mapped=g.map_points(matrix,{count=o.count,x=o.x,y=o.y})
  for c=1,o.contour_count do
    local a,b=o.first[c],o.last[c];local x,y=mapped.x[a],mapped.y[a];local points={x={x},y={y}}
    total=total+1
    for i=a+1,b-1,3 do
      local sample=g.flatten_cubic({x={x,mapped.x[i],mapped.x[i+1],mapped.x[i+2]},
        y={y,mapped.y[i],mapped.y[i+1],mapped.y[i+2]}},{tolerance=.08,max_points=128,max_depth=12})
      total=total+sample.count-1;assert(total<=4096,"Author layer sampling limit reached.")
      for j=2,sample.count do points.x[#points.x+1]=sample.x[j];points.y[#points.y+1]=sample.y[j]end
      x,y=mapped.x[i+2],mapped.y[i+2]
    end
    if #points.x>=3 then contours[#contours+1]=points end
  end
  if #contours==0 then return {row={},first={},last={}}end
  return g.rasterize_region(contours,{columns=SIDE,rows=SIDE},{rule="non_zero"})
end
function M.render(value,budget)
  local input=M.inputs(value)
  assert(budget.catalogs<4,"Four render/catalog attempts used; close and reopen the laboratory.")
  budget.catalogs=budget.catalogs+1
  local catalog,font,mask,result
  local colored={}
  local ok,image,report=pcall(function()
    catalog=plum.text.installed_fonts{query=input.family,max_faces=64,paged=true}
    local selected
    for page=1,4 do
      for i=1,catalog:info().count do local face=catalog:face(i)
        if face.family==input.family and(input.style==""or face.style==input.style)then selected=i;break end
      end
      if selected or not catalog:info().has_more or page==4 then break end
      catalog:next_page()
    end
    assert(selected,"No exact installed family/style in the bounded catalog search; no fallback was chosen.")
    local face=catalog:face(selected);font=catalog:load(selected);catalog:close();catalog=nil
    assert(font:info().outline_format=="glyf","This example requires bounded glyf outlines.")
    local shaped=font:shape_run(input.text)
    assert(shaped.missing_count==0 and shaped.count==1,"The chosen font cannot supply one composed glyph; original input is unchanged.")
    local layers=assert(font:color_layers(shaped.glyph_id[1],{palette=input.palette,max_layers=31}),"No original COLRv0 color layers for this glyph.")
    assert(layers.count>0,"The glyph has no color layers.")
    assert(budget.images+2*layers.count+1<=plum.images.max_handles,"Cumulative native image budget reached; close and reopen before another preview.")
    local outlines={};local bounds
    for i=1,layers.count do
      local o=font:glyph_outline(layers.glyph_id[i],{max_points=2048,max_contours=32});outlines[i]=o
      if o.control_bounds then local b=o.control_bounds
        if not bounds then bounds={b.x_min,b.y_min,b.x_max,b.y_max}
        else bounds={math.min(bounds[1],b.x_min),math.min(bounds[2],b.y_min),math.max(bounds[3],b.x_max),math.max(bounds[4],b.y_max)}end
      end
    end
    assert(bounds,"No supported visible layer outlines; original input is unchanged.")
    local w,h=math.max(1e-9,bounds[3]-bounds[1]),math.max(1e-9,bounds[4]-bounds[2])
    local scale=(SIDE-2)/math.max(w,h)
    local matrix=plum.geometry.mapping{scale_x=scale,scale_y=-scale,
      translate_x=(SIDE-w*scale)/2-bounds[1]*scale,
      translate_y=(SIDE-h*scale)/2+bounds[4]*scale}
    for i=1,layers.count do
      local spans=contour_spans(outlines[i],matrix)
      mask=plum.images.mask_from_spans(spans,{width=SIDE,height=SIDE});budget.images=budget.images+1
      local image=mask:colorize{layers.red[i],layers.green[i],layers.blue[i],layers.alpha[i]}
      budget.images=budget.images+1;mask:close();mask=nil
      colored[#colored+1]={image=image}
    end
    result=plum.images.composite(colored,{width=SIDE,height=SIDE});budget.images=budget.images+1
    return result,{family=face.family,style=face.style,text=input.text,palette=input.palette,
      layer_count=layers.count,table_version=layers.table_version,glyph_id=shaped.glyph_id[1]}
  end)
  if mask then mask:close()end
  for _,layer in ipairs(colored)do layer.image:close()end
  if font then font:close()end
  if catalog then catalog:close()end
  if not ok then if result then result:close()end;error(image,0)end
  return image,report
end
function M.panel()
  assert(plum.has("fonts.list")and plum.has("ui.panels"),"Grant installed-font listing and native plugin panels for this command.")
  local input=defaults();local status="Choose explicit inputs, then Preview. No musical edits."
  local saved=plum.config.get(KEY)
  if saved~=nil then local ok,value=pcall(M.inputs,saved);if ok then input=value else status="Saved inputs invalid; showing defaults without overwriting them."end end
  local budget={catalogs=0,images=0};local image,rendered,capture
  local canvas_w,canvas_h=1,1
  local function preview()
    local candidate,report=M.render(input,budget)
    local ok,ui_image,pixels=pcall(function()
      local pixels=M.capture(candidate,report)
      return plum.ui.image(candidate,{max_side=SIDE,filter="nearest"}),pixels
    end);candidate:close()
    if not ok then error(ui_image,0)end
    if image then image:close()end
    image,rendered,capture=ui_image,report,pixels
    status=string.format("Original %d layer glyphs composited natively. %d/4 render attempts; %d/64 native images.",report.layer_count,budget.catalogs,budget.images)
  end
  local function spec()
    local project=plum.has("project.read")and plum.project.current()~=nil
    local side=math.min(canvas_w,canvas_h);local w,h=side/canvas_w,side/canvas_h
    local x,y=(1-w)/2,(1-h)/2;local primitives={}
    for row=0,3 do for col=0,3 do local c=(row+col)%2==0 and 45 or 85
      primitives[#primitives+1]={kind="rect",x1=x+col*w/4,y1=y+row*h/4,x2=x+(col+1)*w/4,y2=y+(row+1)*h/4,fill=true,color={c,c,c,255}}
    end end
    if image then primitives[#primitives+1]={kind="image",image=image,x1=x,y1=y,x2=x+w,y2=y+h}end
    return {title="Text Art Studio — Color Glyph Laboratory",width=760,height=700,content={id="root",kind="column",children={
      {id="about",kind="label",label="Lua-authored original COLRv0 layer preview. Explicit capture stores project pixels; musical edits use the separate tool."},
      {id="actions",kind="row",children={
        {id="preview",kind="button",label="Preview glyph",enabled=budget.catalogs<4 and budget.images<63},
        {id="save",kind="button",label="Save inputs"},{id="save_preset",kind="button",label="Save preset"},
        {id="load_preset",kind="button",label="Load preset"},{id="close",kind="button",label="Close"}}},
      {id="capture_actions",kind="row",children={
        {id="capture",kind="button",label="Capture for MIDI",enabled=project and capture~=nil},
        {id="save_pixels",kind="button",label="Save pixel preset",enabled=capture~=nil},
        {id="load_pixels",kind="button",label="Load pixels into project",enabled=project}}},
      {id="family",kind="text",label="Exact installed family (128 UTF-8 bytes)",value=input.family},
      {id="style",kind="text",label="Exact style (empty chooses first face in requested family)",value=input.style},
      {id="text",kind="text",label="One original grapheme (64 scalars maximum)",value=input.text},
      {id="palette",kind="number",label="Zero-based palette",min=0,max=65535,step=1,value=input.palette},
      {id="glyph_canvas",kind="canvas",interactive=false,height=260,primitives=primitives},
      {id="rendered",kind="label",label=rendered and string.format("Displayed: %s / %s · %s · palette%d · COLR table%d, original v0 compatibility layers.",rendered.family,rendered.style,rendered.text,rendered.palette,rendered.table_version)or"No glyph displayed yet."},
      {id="status",kind="label",label=status}
    }}}
  end
  local panel=plum.ui.open_panel(spec())
  while true do
    local e=panel:next_event();if not e then if image then image:close()end;return end
    local update=false
    if e.kind=="change"and(e.id=="family"or e.id=="style"or e.id=="text")then input[e.id]=e.value
    elseif e.kind=="change"and e.id=="palette"then input.palette=math.floor(e.value+.5)
    elseif e.kind=="resize"and e.id=="glyph_canvas"and e.pointer then canvas_w,canvas_h=e.pointer.width,e.pointer.height;update=true
    elseif e.kind=="activate"and e.id=="preview"then
      local ok,err=pcall(preview);if not ok then status=tostring(err).." Previous preview retained."end;update=true
    elseif e.kind=="activate"and(e.id=="capture"or e.id=="save_pixels"or e.id=="load_pixels")then
      local ok,err=pcall(function()
        if e.id=="load_pixels"then
          local value=M.validate_capture(assert(plum.presets.load(CAPTURE_PRESET),"No captured pixel preset saved."))
          assert(plum.project.current(),"Open a project first.");plum.config.project.set(CAPTURE,value)
          status="Pixel preset staged for this project on normal Close. Displayed preview is unchanged."
        else
          local value=M.validate_capture(assert(capture,"Preview a glyph first."))
          if e.id=="save_pixels"then plum.presets.save(CAPTURE_PRESET,value);status="Fixed pixels explicitly saved; no font path or handle."
          else assert(plum.project.current(),"Open a project first.");plum.config.project.set(CAPTURE,value)
            status="Displayed pixels staged for this project on normal Close. Then use Custom > Color Glyph Artwork."
          end
        end
      end)
      if not ok then status=tostring(err)end;update=true
    elseif e.kind=="activate"and(e.id=="save"or e.id=="save_preset")then
      local ok,err=pcall(function()local value=M.inputs(input)
        if e.id=="save"then plum.config.set(KEY,value)else plum.presets.save(PRESET,value)end
      end)
      status=ok and"Plain inputs explicitly saved; preview and musical Undo are unchanged."or tostring(err);update=true
    elseif e.kind=="activate"and e.id=="load_preset"then
      local ok,value=pcall(function()return M.inputs(assert(plum.presets.load(PRESET),"No compatible preset saved."))end)
      if ok then input=value;status="Preset restored. Press Preview to update the displayed glyph."else status=tostring(value)end;update=true
    elseif e.kind=="activate"and e.id=="close"then panel:close();if image then image:close()end;return end
    if update then panel:update(spec())end
  end
end
return M
