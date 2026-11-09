use super::*;
use crate::lua::{Host, Limits};
fn host(bytes: usize) -> Host {
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Default::default()
        },
        [],
    )
    .unwrap();
    h.lua.set_app_data(NativeBudget::isolated(bytes));
    h
}
fn run(h: &Host, s: &str) {
    h.run("glyph-outline.lua", s)
        .unwrap_or_else(|e| panic!("{e}"));
}
fn clear(h: &Host) {
    h.lua.gc_collect().unwrap();
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}
#[test]
fn native_cubic_outlines_are_closed_compact_deterministic_and_font_dependent() {
    let h = host(16 * 1024 * 1024);
    run(
        &h,
        r#"
      local a=plum.text.builtin_font();local b=plum.text.builtin_font('monospace')
      assert(a:info().outline_format=='glyf')
      local p=a:glyph_outline(a:glyph_id('O'));local q=b:glyph_outline(b:glyph_id('O'))
      assert(p.status=='outline' and p.format=='cubic' and p.units=='em' and p.count>4 and p.contour_count==2)
      assert(p.count~=q.count or p.x[1]~=q.x[1] or p.y[1]~=q.y[1])
      local same=a:glyph_outline(a:glyph_id('O'));assert(same.count==p.count and same.contour_count==p.contour_count)
      local points=0
      for c=1,p.contour_count do
        local first,last=p.first[c],p.last[c];assert(first>=1 and last<=p.count+1 and (last-first-1)%3==0)
        assert(p.x[first]==p.x[last-1] and p.y[first]==p.y[last-1]);points=points+last-first
        for i=first,last-1 do assert(p.x[i]==same.x[i] and p.y[i]==same.y[i]);assert(p.x[i]>=p.control_bounds.x_min and p.x[i]<=p.control_bounds.x_max);assert(p.y[i]>=p.control_bounds.y_min and p.y[i]<=p.control_bounds.y_max)end
      end
      assert(points==p.count and p.advance==a:glyph_advance(a:glyph_id('O')))
      local accent=a:glyph_outline(a:glyph_id('é'));assert(accent.count>4 and accent.contour_count>0)
      p.x[1]=99;assert(same.x[1]~=99)
    "#,
    );
    clear(&h);
}
#[test]
fn actual_bundled_emoji_has_geometry_and_blank_glyph_is_explicit() {
    let h = host(16 * 1024 * 1024);
    run(
        &h,
        r#"
      local font=plum.text.builtin_font('emoji');local id=assert(font:glyph_id('😀'))
      local glyph=font:glyph_outline(id);assert(glyph.status=='outline' and glyph.count>4 and glyph.contour_count>0)
      local a=plum.text.builtin_font();local space=a:glyph_outline(a:glyph_id(' '));assert(space.status=='no_outline' and space.count==0 and space.contour_count==0 and space.control_bounds==nil and space.advance>0)
      assert(a:glyph_id(0x10ffff)==nil and a:glyph_outline(0).status=='outline')
    "#,
    );
    clear(&h);
}
#[test]
fn glyph_limits_and_bad_inputs_fail_without_partial_tables_or_poisoned_fonts() {
    let h = host(16 * 1024 * 1024);
    run(
        &h,
        r#"
      local f=plum.text.builtin_font();local id=f:glyph_id('O')
      for _,opts in ipairs({{max_points=0},{max_points=65537},{max_points=4.0},{max_contours=0},{max_contours=257},{max_contours=1.0},{unknown=1},setmetatable({},{})})do assert(not pcall(function()f:glyph_outline(id,opts)end))end
      for _,id in ipairs({-1,f:info().glyph_count,1.0,0.5,'1',false,{}})do assert(not pcall(function()f:glyph_outline(id)end))end
      local full=f:glyph_outline(id)
      assert(f:glyph_outline(id,{max_points=full.count,max_contours=full.contour_count}).count==full.count)
      local ok,e=pcall(function()f:glyph_outline(id,{max_points=full.count-1})end);assert(not ok and tostring(e):find('max_points'))
      ok,e=pcall(function()f:glyph_outline(id,{max_contours=1})end);assert(not ok and tostring(e):find('max_contours'))
      assert(f:glyph_id('O')==id and f:glyph_outline(id).count==full.count)
    "#,
    );
    clear(&h);
    let h = host(2 * 1024 * 1024);
    run(
        &h,
        "local f=plum.text.builtin_font();local ok,e=pcall(function()f:glyph_outline(f:glyph_id('O'),{max_points=65536})end);assert(not ok and tostring(e):find('native'));assert(f:glyph_id('A'))",
    );
    clear(&h);
}
#[test]
fn glyph_callback_cancellation_discards_private_contours_and_releases_scratch() {
    let h = host(16 * 1024 * 1024);
    let cancel = h.cancel_handle();
    h.lua
        .globals()
        .set(
            "controlled",
            h.lua
                .create_function(move |lua, (font, id): (AnyUserData, Value)| {
                    let font = font.borrow::<Font>()?;
                    let mut calls = 0;
                    let result = outline_controlled(lua, &font, id, None, || {
                        calls += 1;
                        if calls == 4 {
                            cancel.store(true, std::sync::atomic::Ordering::Release);
                        }
                        notes::check_budget(lua)
                    });
                    assert_eq!(calls, 4);
                    result
                })
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        h.run(
            "cancel.lua",
            "local f=plum.text.builtin_font();controlled(f,f:glyph_id('O'))"
        )
        .unwrap_err()
        .category,
        Category::Canceled
    );
    clear(&h);
    run(
        &h,
        "local f=plum.text.builtin_font();assert(f:glyph_outline(f:glyph_id('O')).count>0)",
    );
    clear(&h);
}
#[test]
fn metadata_font_without_supported_outline_source_is_explicitly_refused() {
    let h = host(16 * 1024 * 1024);
    h.lua
        .globals()
        .set(
            "font_without_outlines",
            h.lua
                .create_function(|lua, ()| {
                    let defs = egui::FontDefinitions::default();
                    let mut bytes = defs.font_data["Ubuntu-Light"].font.to_vec();
                    let count = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize;
                    let mut records = bytes[12..12 + count * 16]
                        .chunks_exact(16)
                        .map(|r| <[u8; 16]>::try_from(r).unwrap())
                        .collect::<Vec<_>>();
                    let record = records.iter_mut().find(|r| &r[..4] == b"glyf").unwrap();
                    record[..4].copy_from_slice(b"ZZZZ");
                    records.sort_unstable_by_key(|r| <[u8; 4]>::try_from(&r[..4]).unwrap());
                    for (i, record) in records.iter().enumerate() {
                        bytes[12 + i * 16..28 + i * 16].copy_from_slice(record);
                    }
                    let (job, budget) = context(lua)?;
                    let allocation = budget
                        .reserve(bytes.len() * 2 + 1024 * 1024)
                        .map_err(|e| e.lua())?;
                    store(
                        lua,
                        job,
                        bytes,
                        0,
                        "selected_file",
                        "no-outline.ttf".into(),
                        allocation,
                    )
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        "local f=font_without_outlines();assert(f:info().outline_format=='none');local id=assert(f:glyph_id('A'));assert(f:glyph_advance(id)>0);local ok,e=pcall(function()f:glyph_outline(id)end);assert(not ok and tostring(e):find('require glyf'))",
    );
    clear(&h);
}
#[test]
fn corrupt_cmap_glyph_reference_fails_explicitly_instead_of_claiming_coverage() {
    let h = host(16 * 1024 * 1024);
    h.lua
        .globals()
        .set(
            "bad_glyph_reference",
            h.lua
                .create_function(|lua, ()| {
                    let defs = egui::FontDefinitions::default();
                    let mut bytes = defs.font_data["Ubuntu-Light"].font.to_vec();
                    let count = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize;
                    let record = bytes[12..12 + count * 16]
                        .chunks_exact(16)
                        .find(|r| &r[..4] == b"maxp")
                        .unwrap();
                    let offset = u32::from_be_bytes(record[8..12].try_into().unwrap()) as usize;
                    bytes[offset + 4..offset + 6].copy_from_slice(&1u16.to_be_bytes());
                    let (job, budget) = context(lua)?;
                    let allocation = budget
                        .reserve(bytes.len() * 2 + 1024 * 1024)
                        .map_err(|e| e.lua())?;
                    store(
                        lua,
                        job,
                        bytes,
                        0,
                        "selected_file",
                        "bad-cmap.ttf".into(),
                        allocation,
                    )
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        "local f=bad_glyph_reference();assert(f:info().glyph_count==1);local ok,e=pcall(function()f:glyph_id('A')end);assert(not ok and tostring(e):find('invalid glyph'))",
    );
    clear(&h);
}
#[test]
fn author_text_art_commands_produce_distinct_fonts_and_deterministic_native_transactions() {
    use crate::lua::{Capability, World};
    use plumspirit_engine::{
        history::{History, HistoryLimits},
        model::{Bpm, PlumDocument, project::KeyRange},
    };
    let manifest = crate::lua::package::Manifest::parse(include_str!(
        "../../examples-lua/text-art-studio/plum-plugin.toml"
    ))
    .unwrap();
    assert_eq!(manifest.id, "plumspirit.example.text-art-studio");
    let source = include_str!("../../examples-lua/text-art-studio/main.lua");
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let mut hashes = Vec::new();
        for command in [
            "outline-proportional",
            "outline-monospace",
            "outline-emoji",
            "fill-proportional",
            "fill-monospace",
            "fill-emoji",
            "shape-proportional",
            "shape-monospace",
            "shape-emoji",
        ] {
            let mut document = PlumDocument::new_project("Glyph art".into(), Bpm::DEFAULT, 960);
            document.project_mut().keys = keys;
            let mut doc = Arc::new(document);
            let mut history = History::new(HistoryLimits::default());
            history.reset(true);
            history.observe_ids(&doc);
            let h = Host::new(
                Limits::default(),
                [Capability::ReadProject, Capability::EditNotes],
            )
            .unwrap();
            h.set_world(Some(World::new(
                Arc::clone(&doc),
                history.incarnations().snapshot(),
            )));
            h.set_plugin(crate::lua::plugin_api::PluginMeta {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                version: "dev".into(),
                root: std::env::temp_dir(),
                data_dir: None,
                config_version: 1,
            })
            .unwrap();
            run(
                &h,
                &format!(
                    "{source}\nregister(plum.plugin);plum.plugin.__phase('ready');plum.plugin.__run('{command}',{{}})"
                ),
            );
            let preview = h.take_commits().unwrap();
            let hash = preview.result().export_fingerprint();
            let baseline: serde_json::Value = serde_json::from_str(include_str!(
                "../../../../docs/v1_0_1_api02/evidence/s16d-legacy-command-baseline.json"
            ))
            .unwrap();
            let expected = baseline["commands"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| {
                    r["command"] == command
                        && r["keys"].as_u64() == Some(u64::from(keys.max_pitch()) + 1)
                })
                .unwrap();
            assert_eq!(
                hash,
                expected["fingerprint"].as_u64().unwrap(),
                "legacy command {command} output changed"
            );
            assert_eq!(
                preview.result().note_count(),
                usize::try_from(expected["notes"].as_u64().unwrap()).unwrap()
            );
            assert_eq!(doc.note_count(), 0);
            assert!(preview.result().note_count() > 0);
            drop(preview);
            h.set_world(Some(World::new(
                Arc::clone(&doc),
                history.incarnations().snapshot(),
            )));
            run(&h, &format!("plum.plugin.__run('{command}',{{}})"));
            let mut commits = h.take_commits().unwrap();
            assert_eq!(commits.result().export_fingerprint(), hash);
            assert_eq!(commits.steps.len(), 1);
            commits.apply(&mut history, &mut doc).unwrap();
            drop(commits);
            assert_eq!(doc.export_fingerprint(), hash);
            history.undo(Arc::get_mut(&mut doc).unwrap()).unwrap();
            assert_eq!(doc.note_count(), 0);
            history.redo(Arc::get_mut(&mut doc).unwrap()).unwrap();
            assert_eq!(doc.export_fingerprint(), hash);
            hashes.push(hash);
        }
        assert_ne!(hashes[0], hashes[1], "font shapes change actual MIDI");
        assert_ne!(hashes[0], hashes[3], "fill differs from outline");
        assert_ne!(hashes[3], hashes[4], "filled fonts differ");
        assert_ne!(hashes[6], hashes[7], "shaped fonts differ");
    }
}

#[test]
fn original_o_contours_fill_together_without_erasing_the_counter() {
    let h = host(16 * 1024 * 1024);
    run(
        &h,
        r#"
      local f=plum.text.builtin_font();local o=f:glyph_outline(assert(f:glyph_id('O')));local g=plum.geometry
      local cs={};for c=1,o.contour_count do
        local a,b=o.first[c],o.last[c];local x,y=o.x[a]*64,o.y[a]*64;local p={x={x},y={y}}
        for i=a+1,b-1,3 do
          local q=g.flatten_cubic({x={x,o.x[i]*64,o.x[i+1]*64,o.x[i+2]*64},y={y,o.y[i]*64,o.y[i+1]*64,o.y[i+2]*64}},{tolerance=0.01})
          for j=2,q.count do p.x[#p.x+1]=q.x[j];p.y[#p.y+1]=q.y[j]end
          x,y=o.x[i+2]*64,o.y[i+2]*64
        end;cs[#cs+1]=p
      end
      local grid={columns=64,rows=64};local r=g.rasterize_region(cs,grid,{rule='non_zero'})
      local cx=math.floor((o.control_bounds.x_min+o.control_bounds.x_max)*32)
      local cy=math.floor((o.control_bounds.y_min+o.control_bounds.y_max)*32)
      local function contains(r,x,y)for i=1,r.count do if r.row[i]==y and r.first[i]<=x and x<r.last[i]then return true end end return false end
      assert(r.cell_count>0 and not contains(r,cx,cy),'O must retain its central hole')
      local mask=plum.images.mask_from_spans(r,{width=64,height=64})
      assert(mask:bounds().count==r.cell_count and mask:pixel(cx,cy)==0)
      for y=0,63 do local row=mask:row(y);for x=0,63 do assert(row:byte(x+1)==(contains(r,x,y)and 255 or 0))end end
      local sampler=mask:note_sampler{start_tick='9007199254740993',end_tick='9007199254741057',first_key=0,last_key=63}
      local coverage,matched=sampler:sample(9007199254740993+cx,1,63-cy)
      assert(coverage==0 and not matched,'Glyph counter must remain absent in note-space sampling')
      mask:close();sampler:close()

      local separately=0;local filled_center=false
      for _,p in ipairs(cs)do local q=g.rasterize_polygon(p,grid);separately=separately+q.cell_count;filled_center=filled_center or contains(q,cx,cy)end
      assert(filled_center and separately>r.cell_count,'separate fills would lose the counter')
      local eo=g.rasterize_region(cs,grid);assert(eo.count==r.count and eo.cell_count==r.cell_count)
      f:close()
    "#,
    );
    clear(&h);
}
