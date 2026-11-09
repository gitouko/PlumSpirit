plum.require_api(0,2)
-- Author-owned state and mathematics; the host supplies native bounded widgets.
local function run()
  local tab,selected,x,y=1,1,.5,.5
  local tone={255,180,60,255}
  local catalog={
    {id="graphs",label="Author graph models",expanded=true,children={
      {id="bezier",label="Bézier model"},{id="polar",label="Polar model"},{id="envelope",label="Envelope model"},
      {id="locked",label="Disabled item",enabled=false},
    }},
    {id="brushes",label="Author brush models",expanded=false,children={
      {id="freehand",label="Freehand model"},{id="eraser",label="Eraser model"},
    }},
  }
  local catalog_value="bezier"
  local function find(items,id)
    for _,item in ipairs(items)do if item.id==id then return item end
      local child=find(item.children or {},id);if child then return child end
    end
  end
  local function restore(v)
    if type(v)~="table" or v.version~=1 or type(v.color)~="table" or #v.color~=4 or not find(catalog,v.selected) or type(v.graphs)~="boolean" or type(v.brushes)~="boolean" then return false end
    for _,c in ipairs(v.color)do if math.type(c)~="integer" or c<0 or c>255 then return false end end
    tone={table.unpack(v.color)};catalog_value=v.selected
    catalog[1].expanded=v.graphs;catalog[2].expanded=v.brushes;return true
  end
  local status="Drag the plot handle; open Data for a512item native list."
  local choices={};for i=1,512 do choices[i]=string.format("Dataset %03d",i)end
  local function spec()
    local points={}
    for i=0,63 do
      local a,b=i/64,(i+1)/64
      points[#points+1]={kind="line",x1=a,y1=.5-.35*math.sin(2*math.pi*a),x2=b,y2=.5-.35*math.sin(2*math.pi*b),color={90,180,245,255}}
    end
    points[#points+1]={kind="circle",x=x,y=y,radius=6,fill=true,color=tone}
    return {title="UI Layout Laboratory",width=660,height=520,content={id="root",kind="column",children={
      {id="tabs",kind="tabs",value=tab,children={
        {id="plot_tab",kind="group",label="Plot",children={{id="plot",kind="canvas",height=200,primitives=points}}},
        {id="data_tab",kind="group",label="Data",children=tab==2 and {
          {id="data",kind="list",label="512 bounded options; only viewport rows are rendered",height=200,choices=choices,value=selected},
        }or{}},
        {id="controls_tab",kind="group",label="Controls",children=tab==3 and {
          {id="catalog",kind="tree",height=140,items=catalog,value=catalog_value},
          {id="tone",kind="color",label="Author RGBA",value=tone},
          {id="gradient",kind="canvas",height=96,interactive=false,primitives={{kind="gradient",x1=0,y1=0,x2=1,y2=1,
            top_left=tone,top_right={30,190,255,255},bottom_left={tone[1],tone[2],tone[3],0},bottom_right={20,20,60,255}}}},
          {id="presets",kind="row",children={
            {id="save_controls",kind="button",label="Save controls"},{id="load_controls",kind="button",label="Load controls"},{id="reset_controls",kind="button",label="Reset controls"},
          }},
        }or{}},
        {id="disabled",kind="group",label="Disabled",enabled=false,children={}},
      }},
      {id="status",kind="label",label=status},
      {id="finish",kind="button",label="Close laboratory"},
    }}}
  end
  local panel=plum.ui.open_panel(spec())
  local held,last_time=false,-1
  while true do
    local e=panel:next_event()
    if not e then return end
    local update=false
    if e.kind=="change" and e.id=="tabs" then
      tab=e.value;status=tab==2 and ("Selected dataset "..selected)or(tab==3 and ("Selected author model "..catalog_value)or"Plot is active.");update=true
    elseif e.kind=="change" and e.id=="data" then
      selected=e.value;status="Selected dataset "..selected;update=true
    elseif e.kind=="change" and e.id=="catalog" then
      if find(catalog,e.value)then catalog_value=e.value;status="Selected author model "..catalog_value;update=true end
    elseif e.kind=="expand" and e.id=="catalog" then
      local item=find(catalog,e.value.id);if item then item.expanded=e.value.expanded;update=true end
    elseif e.kind=="change" and e.id=="tone" then tone=e.value;status="Author RGBA updated.";update=true
    elseif e.kind=="activate" and e.id=="save_controls" then
      plum.presets.save("UI Layout Laboratory",{version=1,color=tone,selected=catalog_value,graphs=catalog[1].expanded,brushes=catalog[2].expanded})
      status="Controls preset saved.";update=true
    elseif e.kind=="activate" and e.id=="load_controls" then
      status=restore(plum.presets.load("UI Layout Laboratory"))and"Controls preset restored."or"No compatible controls preset saved.";update=true
    elseif e.kind=="activate" and e.id=="reset_controls" then
      tone={255,180,60,255};catalog_value="bezier";catalog[1].expanded=true;catalog[2].expanded=false
      status="Controls reset; preset unchanged.";update=true
    elseif e.id=="plot" then
      if e.kind=="pointer_cancel" then held=false end
      if e.kind=="pointer_down" then held=true end
      if held and e.pointer and (e.kind=="pointer_down" or e.kind=="pointer_move" or e.kind=="pointer_up") then
        x=math.max(0,math.min(1,e.pointer.x));y=math.max(0,math.min(1,e.pointer.y))
        status=string.format("Author coordinates %.6f, %.6f",x,y)
        -- Bound publication rate independently of raw pointer input.
        if e.kind=="pointer_down" or e.kind=="pointer_up" or e.pointer.time-last_time>=.05 then last_time=e.pointer.time;update=true end
      end
      if e.kind=="pointer_up" then held=false end
    elseif e.kind=="activate" and e.id=="finish" then panel:close();return end
    if update then panel:update(spec())end
  end
end
function register(plugin)
  plugin.command{id="open",name="Open UI Layout Laboratory",run=run}
end
