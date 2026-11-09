use super::*;
use crate::lua::{Host, Limits};

fn host() -> Host {
    let h = Host::new(
        Limits {
            memory: 8 * 1024 * 1024,
            ..Default::default()
        },
        [],
    )
    .unwrap();
    h.lua.set_app_data(NativeBudget::isolated(2 * 1024 * 1024));
    h
}
fn run(h: &Host, source: &str) {
    h.run("math-test.lua", source).unwrap();
}
fn clear(h: &Host) {
    assert_eq!(h.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn seeded_values_match_native_distributions_and_are_order_page_and_seed_stable() {
    let h = host();
    h.lua
        .globals()
        .set(
            "native_random",
            h.lua
                .create_function(|_, (seed, salt, index, kind): (i64, i64, u64, String)| {
                    let d = match kind.as_str() {
                        "uniform" => Distribution::Uniform,
                        "gaussian" => Distribution::Gaussian,
                        "triangular" => Distribution::Triangular,
                        _ => Distribution::Exponential,
                    };
                    Ok(Rng::for_index(seed as u64, salt as u64, index).bipolar(d))
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        r#"
      local g=plum.geometry;local indices={0,1,7,math.maxinteger,2,7,0}
      for _,seed in ipairs({0,1,-1,math.mininteger,math.maxinteger})do
        for _,salt in ipairs({0,17,-1,math.mininteger})do
          for _,kind in ipairs({'uniform','gaussian','triangular','exponential'})do
            local opts={salt=salt,distribution=kind};local a=g.random_values(seed,indices,opts)
            local b=g.random_values(seed,{math.maxinteger,7,0},opts)
            assert(#a==7 and a[4]==b[1]and a[3]==b[2]and a[1]==b[3]and a[3]==a[6]and a[1]==a[7])
            for i,index in ipairs(indices)do assert(a[i]==native_random(seed,salt,index,kind)and a[i]>=-1 and a[i]<=1)end
          end
        end
      end
      local ts={};for i=1,4096 do ts[i]=i end
      local a=g.random_values(712,ts);local b=g.random_values(713,ts);local c=g.random_values(712,ts,{salt=1})
      local same=0;for i=1,4096 do if a[i]==b[i]or a[i]==c[i]then same=same+1 end end;assert(same==0)
      local p=g.random_values(712,{1,2,3});for i=1,3 do assert(a[i]==p[i])end
      assert(indices[4]==math.maxinteger and indices[6]==7)
    "#,
    );
    clear(&h);
}

#[test]
fn authored_parametric_formulas_match_known_values_and_native_expression_semantics() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local ts={-2,-1,-0.25,0,0.25,1,2,0.25}
      local p=g.formula_points(ts,{x='2*t+3',y='sin(2*pi*t)+cos(pi*t)'})
      assert(p.count==8)
      for i,t in ipairs(ts)do assert(p.x[i]==2*t+3 and math.abs(p.y[i]-(math.sin(2*math.pi*t)+math.cos(math.pi*t)))<1e-12)end
      assert(p.x[5]==p.x[8]and p.y[5]==p.y[8])
      local polar=g.formula_points({0,0.25,0.5,0.75,1},{x='3*cos(2*pi*t)',y='3*sin(2*pi*t)'})
      assert(polar.x[1]==3 and polar.y[1]==0 and math.abs(polar.x[2])<1e-12 and polar.y[2]==3)
      local negative=g.formula_points({-1,-17},{x='t % 16',y='t // 16'});assert(negative.x[1]==15 and negative.y[1]==-1 and negative.x[2]==15 and negative.y[2]==-2)
      local e=g.formula_points({1,2,3},{x='math.log(t,2)',y='math.sqrt(t*t)'});assert(e.x[1]==0 and e.x[2]==1 and e.y[3]==3)
      local max={};for i=1,4096 do max[i]=(i-1)/4095 end
      local out=g.formula_points(max,{x='t',y='t*t'});assert(out.count==4096 and out.x[1]==0 and out.x[4096]==1 and out.y[4096]==1)
      local mapped=g.map_points(g.mapping{translate_x=30,scale_y=-2},p);assert(mapped.x[1]==p.x[1]+30 and mapped.y[1]==-2*p.y[1])
      assert(ts[1]==-2 and ts[8]==0.25)
    "#,
    );
    clear(&h);
}

#[test]
fn strict_random_and_formula_inputs_and_all_math_domain_errors_are_atomic() {
    let h = host();
    run(
        &h,
        r#"
      local g=plum.geometry;local ids={0,1,2};local ts={0,0.5,1};local formula={x='t',y='sin(t)'}
      for _,seed in ipairs({1.0,1.5,'1',false,{},0/0,math.huge})do assert(not pcall(g.random_values,seed,ids))end
      for _,bad in ipairs({-1,1.0,0/0,math.huge,'1',false,{}})do assert(not pcall(g.random_values,0,{0,bad,2}))end
      for _,kind in ipairs({'normal','Uniform',false,1,string.rep('x',33)})do assert(not pcall(g.random_values,0,ids,{distribution=kind}))end
      for _,opts in ipairs({{salt=1.0},{salt='1'},{extra=1},setmetatable({},{})})do assert(not pcall(g.random_values,0,ids,opts))end
      for _,values in ipairs({{},setmetatable({0,1},{}),{0,1,extra=1}})do assert(not pcall(g.random_values,0,values));assert(not pcall(g.formula_points,values,formula))end
      local huge={};for i=1,4097 do huge[i]=i end;assert(not pcall(g.random_values,0,huge));assert(not pcall(g.formula_points,huge,formula))
      for _,t in ipairs({0/0,math.huge,1e13,'1',false,{}})do assert(not pcall(g.formula_points,{0,t,1},formula))end
      for _,f in ipairs({'1/0','sqrt(-1)','ln(0)','t/(t-t)','exp(10000)','1e300*1e300','math.random()','os.execute(1)','while true do end','function()end','t=4','other','t..t','1;2','',string.rep('1',4001),string.rep('(',65)..'t'..string.rep(')',65),string.rep('1+',256)..'1'})do
        local ok,e=pcall(g.formula_points,ts,{x='t',y=f});assert(not ok,tostring(f))
      end
      for _,o in ipairs({{x='t'},{x='t',y=1},{x='t',y='t',extra=1},setmetatable({x='t',y='t'},{})})do assert(not pcall(g.formula_points,ts,o))end
      assert(not pcall(g.formula_points,{1e12},{x='t*t',y='t'}))
      assert(ids[3]==2 and ts[2]==0.5 and formula.y=='sin(t)')
      assert(g.formula_points(ts,formula).count==3 and #g.random_values(0,ids)==3)
    "#,
    );
    clear(&h);
}

#[test]
fn native_preflight_and_deep_random_formula_cancellation_release_private_workspace() {
    let h = host();
    h.lua.set_app_data(NativeBudget::isolated(10));
    for s in [
        "plum.geometry.random_values(1,{0,1,2})",
        "plum.geometry.formula_points({0,1},{x='t',y='t'})",
    ] {
        assert_eq!(
            h.run("quota.lua", s).unwrap_err().category,
            Category::Budget
        );
        clear(&h);
    }
    h.lua.set_app_data(NativeBudget::isolated(2 * 1024 * 1024));
    for random in [true, false] {
        let cancel = h.cancel_handle();
        h.lua
            .globals()
            .set(
                "controlled_math",
                h.lua
                    .create_function(move |lua, t: Table| {
                        let mut calls = 0;
                        let check = || {
                            calls += 1;
                            if calls == 5 {
                                cancel.store(true, std::sync::atomic::Ordering::Release);
                            }
                            crate::lua::notes::check_budget(lua)
                        };
                        if random {
                            random_controlled(lua, Value::Integer(77), &t, None, check)
                        } else {
                            let o = lua.create_table()?;
                            o.set("x", "sin(t)")?;
                            o.set("y", "cos(t)")?;
                            formula_controlled(lua, &t, &o, check)
                        }
                    })
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            h.run(
                "math-cancel.lua",
                "local t={};for i=1,4096 do t[i]=i end;controlled_math(t)"
            )
            .unwrap_err()
            .category,
            Category::Canceled
        );
        clear(&h);
        run(
            &h,
            "assert(#plum.geometry.random_values(77,{1,2,3})==3 and plum.geometry.formula_points({0,1},{x='t',y='t'}).count==2)",
        );
        clear(&h);
    }
}

#[test]
fn actual_vm_output_refusals_leave_random_and_formula_inputs_usable() {
    let h = host();
    h.lua
        .globals()
        .set(
            "limit_vm",
            h.lua
                .create_function(|lua, ()| {
                    lua.gc_stop();
                    lua.set_memory_limit(lua.used_memory() + 4096)?;
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    h.lua
        .globals()
        .set(
            "restore_vm",
            h.lua
                .create_function(|lua, ()| {
                    lua.set_memory_limit(8 * 1024 * 1024)?;
                    lua.gc_restart();
                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        r#"
      local g=plum.geometry;local t={};for i=1,4096 do t[i]=i end;local o={x='t',y='sin(t)'}
      limit_vm();local ok=pcall(g.random_values,12,t);restore_vm();assert(not ok and t[4096]==4096);assert(#g.random_values(12,t)==4096)
      limit_vm();ok=pcall(g.formula_points,t,o);restore_vm();assert(not ok and t[4096]==4096 and o.y=='sin(t)');assert(g.formula_points(t,o).count==4096)
    "#,
    );
    clear(&h);
}

#[test]
fn uniform_reference_vectors_from_independent_unsigned_splitmix_arithmetic() {
    let h = host();
    run(
        &h,
        r#"
      assert(plum.geometry.random_values(0,{0},{salt=0})[1]==0.16301185331250401)
      assert(plum.geometry.random_values(1,{7},{salt=17})[1]==-0.45856579987229873)
      assert(plum.geometry.random_values(-1,{math.maxinteger},{salt=-1})[1]==0.45563638309868915)
      assert(plum.geometry.random_values(math.mininteger,{42},{salt=0})[1]==-0.35356507104325474)
    "#,
    );
    clear(&h);
}

#[test]
fn published_authored_parametric_random_melody_has_preview_apply_one_undo_redo_in_both_key_modes() {
    use crate::lua::{Capability, World};
    use plumspirit_engine::{
        history::{History, HistoryLimits},
        model::{Bpm, PlumDocument, project::KeyRange},
    };
    use std::sync::Arc;
    let source = include_str!("../../../../docs/LUA_API.md")
        .split("### Seeded randomness and authored parametric formulas (API 0.2)")
        .nth(1)
        .unwrap()
        .split("```lua\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    for keys in [KeyRange::Standard, KeyRange::Extended256] {
        let mut d = PlumDocument::new_project("Parametric example".into(), Bpm::DEFAULT, 960);
        d.project_mut().keys = keys;
        let mut doc = Arc::new(d);
        let mut history = History::new(HistoryLimits::default());
        history.reset(false);
        history.observe_ids(&doc);
        let h = Host::new(
            Limits {
                memory: 8 * 1024 * 1024,
                ..Default::default()
            },
            [Capability::ReadProject, Capability::EditNotes],
        )
        .unwrap();
        h.lua.set_app_data(NativeBudget::isolated(8 * 1024 * 1024));
        let bind = |h: &Host, doc: &Arc<PlumDocument>, history: &History| {
            h.set_world(Some(World::new(
                Arc::clone(doc),
                history.incarnations().snapshot(),
            )))
        };
        bind(&h, &doc, &history);
        h.run("parametric-preview.lua", source).unwrap();
        let preview = h.take_commits().unwrap();
        let count = preview.result().note_count();
        assert_eq!(count, 32);
        let expected = preview.result().export_fingerprint();
        assert_eq!(doc.note_count(), 0);
        drop(preview);
        bind(&h, &doc, &history);
        h.run("parametric-apply.lua", source).unwrap();
        let mut commits = h.take_commits().unwrap();
        assert_eq!(commits.result().export_fingerprint(), expected);
        assert_eq!(commits.steps[0].operations, 1);
        h.set_world(None);
        commits.apply(&mut history, &mut doc).unwrap();
        drop(commits);
        assert_eq!(doc.note_count(), count);
        history
            .undo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(doc.note_count(), 0);
        assert!(!history.can_undo());
        history
            .redo(Arc::get_mut(&mut doc).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(doc.export_fingerprint(), expected);
        clear(&h);
    }
}
