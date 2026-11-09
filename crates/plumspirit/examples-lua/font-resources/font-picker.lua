-- Reusable author-owned selector. All controls/rendering use public native APIs.
-- This preview samples ONE Unicode scalar's supported glyf outline, not shaped text.
local function preference(v)
  if type(v)~="table" or type(v.family)~="string" or type(v.style)~="string"
    or #v.family==0 or #v.family>2048 or #v.style>2048
    or not utf8.len(v.family) or not utf8.len(v.style) then return nil end
  if v.version~=nil and v.version~=1 then return nil end
  if v.face_index~=nil and (math.type(v.face_index)~="integer" or v.face_index<0 or v.face_index>255)then return nil end
  return {version=1,family=v.family,style=v.style,face_index=v.face_index}
end
local function same(a,b)
  return a and b and a.family==b.family and a.style==b.style and (a.face_index==nil or a.face_index==b.face_index)
end
local function short(s,n)
  local last=utf8.offset(s,n+1)
  return last and s:sub(1,last-1).."…"or s
end
local function glyph_preview(font,scalar)
  assert(utf8.len(scalar)==1,"Preview needs exactly one Unicode scalar; the original input is preserved.")
  local id=assert(font:glyph_id(scalar),"This face has no glyph for that scalar; no replacement was invented.")
  local outline=font:glyph_outline(id,{max_points=2048,max_contours=32})
  local advance=font:glyph_advance(id)
  if outline.count==0 then return {},string.format("Glyph %d has no outline (for example a space); advance %.6f em.",id,advance)end
  local b=outline.control_bounds
  local width,height=math.max(1e-9,b.x_max-b.x_min),math.max(1e-9,b.y_max-b.y_min)
  local function point(i)return .05+.9*(outline.x[i]-b.x_min)/width,.95-.9*(outline.y[i]-b.y_min)/height end
  local primitives,total={},0
  for c=1,outline.contour_count do
    local first,last=outline.first[c],outline.last[c]
    local x,y=point(first);local xs,ys={x},{y}
    total=total+1;assert(total<=2048,"Author preview sampling limit reached.")
    for i=first+1,last-1,3 do
      local a,b=point(i);local d,e=point(i+1);local f,g=point(i+2)
      local sampled=plum.geometry.flatten_cubic({x={x,a,d,f},y={y,b,e,g}},
        {tolerance=.0025,max_points=64,max_depth=12})
      total=total+sampled.count-1;assert(total<=2048,"Author preview sampling limit reached.")
      for j=2,sampled.count do xs[#xs+1]=sampled.x[j];ys[#ys+1]=sampled.y[j]end
      x,y=f,g
    end
    if #xs>=2 then primitives[#primitives+1]={kind="polyline",x=xs,y=ys,closed=true,width=1.5,color={140,200,255,255}}end
  end
  return primitives,string.format("Glyph %d · advance %.6f em · %d sampled points. Outline preview is fitted independently in X/Y.",id,advance,total)
end
return function(inspect)
  assert(plum.has("fonts.list") and plum.has("ui.panels"),"Grant installed-font listing and native plugin panels to open this chooser.")
  local wanted=preference(plum.config.get("font_choice"))
  local query,scalar="","A"
  local catalog,inventory,faces,choices,selected
  local scans,pages,loads=0,0,0
  local primitives,preview={},"Select a face, then explicitly preview one scalar or choose it."
  local status="No choice is saved until Choose. Cancel leaves the previous choice and report intact."
  local function fill()
    inventory=catalog:info();faces={};choices={};selected=1
    for i=1,inventory.count do
      local face=catalog:face(i);faces[i]=face
      choices[i]=short(face.family,80).." · "..short(face.style,64)..string.format(" · weight %d%s · face %d",face.weight,face.italic and " italic"or"",face.face_index)
      if same(wanted,face)then selected=i end
    end
    if #choices==0 then choices[1]="No matching faces in this page."end
    primitives={};preview="Select a face, then explicitly preview one scalar or choose it."
    local found=false;for _,face in ipairs(faces)do found=found or same(wanted,face)end
    status=string.format("Page %d: %d faces. %s%s",inventory.page,inventory.count,
      inventory.scan_truncated and "Scan reached a published source/name limit. "or"",
      wanted and not found and "Saved choice is not in this page; search or continue. No fallback was saved."or"Choose saves only the explicitly selected face.")
  end
  local function search()
    assert(#query<=128,"Search is limited to 128 UTF-8 bytes.")
    assert(scans<4,"Four catalog scans used; close and reopen the chooser to search again.")
    local next_catalog=plum.text.installed_fonts{query=query,max_faces=64,paged=true}
    if catalog then catalog:close()end
    catalog=next_catalog;scans=scans+1;pages=pages+1;fill()
  end
  search()
  local function spec()
    local face=faces[selected]
    return {title="Installed Font Chooser",width=760,height=650,content={id="font_root",kind="column",children={
      {id="intro",kind="label",label="Author-written selector using native catalog pages, controls and canvas. No musical edits."},
      {id="search_row",kind="row",children={
        {id="query",kind="text",label="Family/style search (128 UTF-8 bytes)",value=query},
        {id="search",kind="button",label="Search",enabled=scans<4 and pages<plum.text.max_font_pages},
        {id="next",kind="button",label="Next page",enabled=inventory.has_more and pages<plum.text.max_font_pages},
      }},
      {id="actions",kind="row",children={
        {id="choose",kind="button",label="Choose",enabled=face~=nil and loads<32},
        {id="cancel",kind="button",label="Cancel"},
        {id="save_preset",kind="button",label="Save choice preset",enabled=face~=nil},
        {id="load_preset",kind="button",label="Load choice preset"},
      }},
      {id="faces",kind="list",label="64 bounded rows per page; native list renders only visible rows",height=170,choices=choices,value=selected},
      {id="details",kind="label",label=face and string.format("%s · %s · weight %d · italic %s · collection face %d · %d bytes",
        short(face.family,80),short(face.style,64),face.weight,tostring(face.italic),face.face_index,face.bytes)or"No selectable face in this page."},
      {id="preview_row",kind="row",children={
        {id="scalar",kind="text",label="One Unicode scalar",value=scalar},
        {id="preview",kind="button",label="Preview outline",enabled=face~=nil and loads<31,tooltip="At most31preview loads; one final load is reserved for Choose."},
      }},
      {id="glyph",kind="canvas",interactive=false,height=150,primitives=primitives},
      {id="preview_status",kind="label",label=preview},
      {id="status",kind="label",label=status},
    }}}
  end
  local panel=plum.ui.open_panel(spec())
  while true do
    local event=panel:next_event()
    if not event then catalog:close();return end
    local refresh=true
    if event.kind=="change" and event.id=="query"then query=event.value;refresh=false
    elseif event.kind=="change" and event.id=="scalar"then scalar=event.value;refresh=false
    elseif event.kind=="change" and event.id=="faces"then
      selected=event.value;primitives={};preview="Selection changed. Preview or Choose explicitly; no font handle is retained."
    elseif event.kind=="activate" and event.id=="search"then
      local ok,e=pcall(search);if not ok then status=tostring(e)end
    elseif event.kind=="activate" and event.id=="next"then
      local ok,e=pcall(function()pages=pages+1;catalog:next_page();fill()end);if not ok then status=tostring(e)end
    elseif event.kind=="activate" and event.id=="preview"and faces[selected]then
      loads=loads+1
      local font
      local ok,a,b=pcall(function()font=catalog:load(selected);return glyph_preview(font,scalar)end)
      if font then font:close()end
      if ok then primitives,preview=a,b else primitives={};preview=tostring(a)end
    elseif event.kind=="activate" and event.id=="save_preset"and faces[selected]then
      local face=faces[selected]
      plum.presets.save("Installed Font Choice",{version=1,family=face.family,style=face.style,face_index=face.face_index})
      status="Plain font preference preset saved; the active choice and report are unchanged."
    elseif event.kind=="activate" and event.id=="load_preset"then
      wanted=preference(plum.presets.load("Installed Font Choice"))
      if wanted then fill()else status="No compatible font preference preset is available."end
    elseif event.kind=="activate" and event.id=="choose"and faces[selected]then
      local font;loads=loads+1
      local ok,report=pcall(function()font=catalog:load(selected);return inspect(font)end)
      if font then font:close()end
      if ok then
        local face=faces[selected]
        -- A single native settings write keeps choice+its report together.
        local choice={version=1,family=face.family,style=face.style,face_index=face.face_index,report=report}
        plum.config.set("font_choice",choice)
        catalog:close();panel:close();return preference(choice)
      else status=tostring(report)end
    elseif event.kind=="activate" and event.id=="cancel"then catalog:close();panel:close();return
    else refresh=false end
    if refresh then panel:update(spec())end
  end
end
