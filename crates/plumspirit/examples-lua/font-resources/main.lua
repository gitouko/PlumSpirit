plum.require_api(0,2)
-- Metrics/coverage are deliberately separate from grapheme shaping and artwork.
local function inspect(font)
  local report={font=font:info(),coverage={}}
  for _,text in ipairs({"A","W","é","Σ","♯","😀"})do
    local id=font:glyph_id(text)
    report.coverage[#report.coverage+1]={text=text,glyph=id,missing=id==nil,
      advance=id and font:glyph_advance(id) or nil}
  end
  font:close()
  return report
end
function register(p)
  p.command{id="inspect-bundled",name="Inspect Bundled Font Metrics",run=function()
    local a=inspect(plum.text.builtin_font())
    local b=inspect(plum.text.builtin_font("monospace"))
    plum.config.set("last_report",{a,b})
    plum.ui.notify(a.font.source_name.." and "..b.font.source_name..": real metrics/coverage saved in plugin config.")
  end}
  p.command{id="inspect-selected",name="Inspect Selected Font…",run=function()
    local file=plum.files.open{filters={{name="OpenType font",extensions={"ttf","otf","ttc"}}}}
    if not file then return end
    local font=plum.text.load_font(file,0)
    file:close()
    local report=inspect(font)
    plum.config.set("last_report",report)
    plum.ui.notify(report.font.source_name..": "..report.font.glyph_count.." glyphs; metrics/coverage saved in plugin config.")
  end}
  p.command{id="inspect-installed",name="Inspect Installed Font Choice",run=function()
    local catalog=plum.text.installed_fonts()
    local inventory=catalog:info()
    if inventory.count==0 then catalog:close();plum.ui.notify("No supported installed font was found in this bounded scan.");return end
    local wanted=plum.config.get("font_choice")
    local selected
    if type(wanted)=="table"then
      for i=1,inventory.count do
        local face=catalog:face(i)
        if face.family==wanted.family and face.style==wanted.style then selected=i;break end
      end
    end
    local fallback=wanted~=nil and selected==nil
    selected=selected or 1
    local face=catalog:face(selected)
    local report=inspect(catalog:load(selected))
    catalog:close()
    report.inventory=inventory;report.choice={family=face.family,style=face.style};report.fallback=fallback
    plum.config.set("font_choice",report.choice)
    plum.config.set("last_report",report)
    plum.ui.notify((fallback and "Requested font unavailable in this scan; using " or "Selected ")..face.family.." "..face.style..(inventory.truncated and " (inventory truncated)." or "."))
  end}
  p.command{id="choose-installed",name="Choose Installed Font…",run=function()
    return plum.plugin.include("font-picker.lua")(inspect)
  end}

end
