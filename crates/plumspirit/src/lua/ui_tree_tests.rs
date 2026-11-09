use super::tests::{clear, driver, host, run};
use super::*;
#[test]
fn tree_color_gradient_schemas_enforce_global_counts_hidden_depth_plain_values_and_ids() {
    let (h, _) = host(true, true);
    h.lua
        .globals()
        .set(
            "validate",
            h.lua
                .create_function(|lua, t: Table| schema::parse(lua, t).map(|_| true))
                .unwrap(),
        )
        .unwrap();
    run(
        &h,
        r#"
      local function spec(n)return{content=n}end
      local function item(id)return{id=id,label='Item '..id,children={}}end
      local function tree(items,value)return{id='tree',kind='tree',items=items,value=value}end
      local leaves={};for i=1,512 do leaves[i]=item('i'..i)end
      assert(validate(spec(tree(leaves,'i512'))))
      local roots={{id='t1',kind='tree',items=leaves},{id='t2',kind='tree',items={item('overflow')}}}
      assert(not pcall(validate,spec{id='root',kind='column',children=roots}))
      leaves[513]=item('i513');assert(not pcall(validate,spec(tree(leaves))))
      local deep=item('8');for i=7,1,-1 do deep={id='n'..i,label='Deep',expanded=false,children={deep}}end
      assert(validate(spec(tree{deep})))
      deep={id='too_deep',label='Hidden',children={deep}};assert(not pcall(validate,spec(tree{deep})))
      for _,node in ipairs{
        tree({item('same'),item('same')}),tree({item('bad id')}),tree({item('') }),
        tree({{id='branch',label='B',children={item('branch')}}}),
        tree({{id='empty',label=''}}),tree({{id='a',label=string.rep('x',257)}}),
        tree({setmetatable(item('a'),{})}),tree(setmetatable({item('a')},{})),
        tree({{id='a',label='A',expanded=1}}),tree({{id='a',label='A',enabled='true'}}),
        tree({item('a')},'missing'),tree({item('a')},1),
        {id='tree',kind='tree',items={item('a')},height=63},
        {id='color',kind='color',value={0,1,2,256}},
        {id='color',kind='color',value={0,1,2,-1}},
        {id='color',kind='color',value={0,1,2,1.0}},
        {id='color',kind='color',value=setmetatable({0,1,2,255},{})},
        {id='color',kind='color',value={1,2,3}},
      }do assert(not pcall(validate,spec(node)))end
      assert(validate(spec(tree({},''))))
      assert(validate(spec{id='color',kind='color',value={1,2,3,0}}))
      assert(validate(spec{id='color',kind='color'}))
      local g={kind='gradient',x1=0,y1=0,x2=1,y2=1,top_left={255,0,0,255},top_right={0,255,0,255},bottom_left={0,0,255,128},bottom_right={0,0,0,0}}
      local function canvas()return spec{id='c',kind='canvas',primitives={g}}end
      assert(validate(canvas()))
      g.top_left=nil;assert(not pcall(validate,canvas()));g.top_left={255,0,0,255}
      g.bottom_left={1,2,3,256};assert(not pcall(validate,canvas()));g.bottom_left={0,0,255,128}
      g.x2=0;assert(not pcall(validate,canvas()));g.x2=1
      g.unknown=true;assert(not pcall(validate,canvas()))
    "#,
    );
    clear(&h);
}
#[test]
fn typed_rgba_changes_coalesce_but_tree_expansion_boundaries_remain_ordered_and_independent() {
    let (h, rx) = host(true, true);
    driver(&h, rx, |s| {
        for color in [[1, 2, 3, 0], [255, 128, 32, 0]] {
            assert!(s.push(Event {
                kind: "change",
                id: "tone".into(),
                value: InputValue::Color(color),
                pointer: None,
                revision: 1,
                sequence: 0
            }));
        }
        for expanded in [true, false] {
            assert!(s.push(Event {
                kind: "expand",
                id: "tree".into(),
                value: InputValue::Expansion {
                    item: "branch".into(),
                    expanded
                },
                pointer: None,
                revision: 1,
                sequence: 0
            }));
        }
        Ok(())
    });
    run(
        &h,
        r#"local p=plum.ui.open_panel{content={id='tone',kind='color'}};drive(p)
      local a=p:next_event(0);assert(a.kind=='change' and a.sequence==2 and #a.value==4 and a.value[1]==255 and a.value[2]==128 and a.value[4]==0)
      a.value[1]=0
      local b=p:next_event(0);assert(b.kind=='expand' and b.sequence==3 and b.value.id=='branch' and b.value.expanded==true)
      b.value.id='mutated';b.value.expanded=false
      local c=p:next_event(0);assert(c.kind=='expand' and c.sequence==4 and c.value.id=='branch' and c.value.expanded==false)
      p:close()
    "#,
    );
    clear(&h);
    let (h, rx) = host(true, true);
    driver(&h, rx, |s| {
        assert!(!s.push(Event {
            kind: "expand",
            id: "tree".into(),
            value: InputValue::Expansion {
                item: "invalid id".into(),
                expanded: true
            },
            pointer: None,
            revision: 1,
            sequence: 0
        }));
        Ok(())
    });
    assert_eq!(
        h.run(
            "bad-tree-event.lua",
            "local p=plum.ui.open_panel{content={id='x',kind='label'}};drive(p);p:next_event(0)"
        )
        .unwrap_err()
        .category,
        Category::Budget
    );
    clear(&h);
}

#[test]
fn layout_laboratory_tree_rgba_preset_reset_and_restore_are_author_owned() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let p = self.0.canonicalize().unwrap();
            let base = std::env::temp_dir().canonicalize().unwrap();
            assert!(
                p.starts_with(&base)
                    && p != base
                    && p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("plum-api02-tree-")
            );
            std::fs::remove_dir_all(p).unwrap();
        }
    }
    let temp = Temp(std::env::temp_dir().join(format!(
        "plum-api02-tree-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir(&temp.0).unwrap();
    let (h, rx) = host(true, true);
    h.set_plugin(crate::lua::plugin_api::PluginMeta {
        id: "script".into(),
        name: "Layout example".into(),
        version: "0.2.0".into(),
        root: temp.0.clone(),
        data_dir: Some(temp.0.join("data")),
        config_version: 1,
    })
    .unwrap();
    h.lua.set_app_data(NativeBudget::isolated(32 * 1024 * 1024));
    let driver = std::thread::spawn(move || {
        let shared = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut revision = 0;
        for step in 0..8 {
            let start = std::time::Instant::now();
            let snapshot = loop {
                if let Some(s) = shared.snapshot()
                    && s.revision > revision
                {
                    break s;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "no layout update step{step}"
                );
                std::thread::sleep(Duration::from_millis(1));
            };
            revision = snapshot.revision;
            fn node<'a>(n: &'a schema::Node, id: &str) -> Option<&'a schema::Node> {
                if n.id == id {
                    return Some(n);
                }
                match &n.kind {
                    schema::Kind::Column(c)
                    | schema::Kind::Row(c)
                    | schema::Kind::Group(c)
                    | schema::Kind::Tabs { children: c, .. } => c.iter().find_map(|n| node(n, id)),
                    _ => None,
                }
            }
            if step > 0 {
                let schema::Kind::Tree {
                    items, selected, ..
                } = &node(&snapshot.root, "catalog").unwrap().kind
                else {
                    panic!()
                };
                let schema::Kind::Color(color) = &node(&snapshot.root, "tone").unwrap().kind else {
                    panic!()
                };
                if step >= 2 && step != 6 {
                    assert!(items[1].expanded);
                }
                if step >= 3 && step != 6 {
                    assert_eq!(*color, [10, 20, 200, 128]);
                }
                if step >= 4 && step != 6 {
                    assert_eq!(selected, "polar");
                }
                if step == 6 {
                    assert_eq!(*color, [255, 180, 60, 255]);
                    assert_eq!(selected, "bezier");
                    assert!(!items[1].expanded);
                }
            }
            let (kind, id, value) = match step {
                0 => ("change", "tabs", InputValue::Choice(2)),
                1 => (
                    "expand",
                    "catalog",
                    InputValue::Expansion {
                        item: "brushes".into(),
                        expanded: true,
                    },
                ),
                2 => ("change", "tone", InputValue::Color([10, 20, 200, 128])),
                3 => ("change", "catalog", InputValue::Text("polar".into())),
                4 => ("activate", "save_controls", InputValue::None),
                5 => ("activate", "reset_controls", InputValue::None),
                6 => ("activate", "load_controls", InputValue::None),
                _ => ("activate", "finish", InputValue::None),
            };
            assert!(shared.push(Event {
                kind,
                id: id.into(),
                value,
                pointer: None,
                revision,
                sequence: 0
            }));
        }
    });
    let source = include_str!("../../examples-lua/ui-layout-laboratory/main.lua");
    let result=h.run("layout-laboratory.lua",&format!("{source}\nregister(plum.plugin);plum.plugin.__phase('ready');plum.plugin.__run('open',{{}})"));
    driver.join().unwrap();
    result.unwrap();
    assert!(h.take_commits().is_none());
    clear(&h);
    let saved: serde_json::Value = serde_json::from_slice(
        &std::fs::read(temp.0.join("data/presets/UI Layout Laboratory.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["value"]["version"], 1);
    assert_eq!(
        saved["value"]["color"],
        serde_json::json!([10, 20, 200, 128])
    );
    assert_eq!(saved["value"]["selected"], "polar");
    assert_eq!(saved["value"]["brushes"], true);
}
