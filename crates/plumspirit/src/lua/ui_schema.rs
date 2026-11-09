//! Plain bounded declarative UI data. No Lua objects survive parsing.
use super::*;
use mlua::{Table, Value};
use std::collections::BTreeSet;

pub const MAX_NODES: usize = 256;
pub const MAX_PRIMITIVES: usize = 2048;
pub const MAX_POINTS: usize = 8192;
pub const MAX_TEXT: usize = 65536;
pub const MAX_OPTIONS: usize = 32768;
pub const MAX_TREE_ITEMS: usize = 512;
pub const SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
pub struct PrimitiveList {
    pub items: Vec<Primitive>,
    pub _allocation: Reservation,
}
pub(crate) fn parse_primitives(lua: &Lua, table: Table) -> mlua::Result<PrimitiveList> {
    notes::check_budget(lua)?;
    let allocation = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("UI accounting unavailable."))?
        .reserve(SNAPSHOT_BYTES)
        .map_err(|e| e.lua())?;
    let mut parser = Parser::new(lua);
    let items = parser.primitives(table)?;
    parser.check()?;
    Ok(PrimitiveList {
        items,
        _allocation: allocation,
    })
}
pub struct Snapshot {
    pub revision: u64,
    pub title: String,
    pub width: f32,
    pub height: f32,
    pub root: Node,
    pub text_bytes: usize,
    pub _allocation: Reservation,
}
pub struct Node {
    pub id: String,
    pub label: String,
    pub tip: String,
    pub enabled: bool,
    pub kind: Kind,
}
pub struct TreeItem {
    pub id: String,
    pub label: String,
    pub tip: String,
    pub enabled: bool,
    pub expanded: bool,
    pub children: Vec<TreeItem>,
}
pub enum Kind {
    Column(Vec<Node>),
    Row(Vec<Node>),
    Group(Vec<Node>),
    Tabs {
        children: Vec<Node>,
        selected: usize,
    },
    Label,
    Button,
    Toggle(bool),
    Color([u8; 4]),
    Tree {
        items: Vec<TreeItem>,
        selected: String,
        height: f32,
    },
    Number {
        value: f64,
        min: f64,
        max: f64,
        step: f64,
        slider: bool,
    },
    Text(String),
    Choice {
        choices: Vec<String>,
        selected: usize,
    },
    List {
        choices: Vec<String>,
        selected: usize,
        height: f32,
    },
    Canvas {
        height: f32,
        interactive: bool,
        primitives: Vec<Primitive>,
    },
}
pub enum Primitive {
    Gradient {
        min: [f64; 2],
        max: [f64; 2],
        colors: [[u8; 4]; 4],
    },
    Image {
        resource: Arc<images::Resource>,
        min: [f64; 2],
        max: [f64; 2],
        uv_min: [f64; 2],
        uv_max: [f64; 2],
        color: [u8; 4],
    },
    Line {
        points: Vec<[f64; 2]>,
        color: [u8; 4],
        width: f32,
        closed: bool,
    },
    Rect {
        min: [f64; 2],
        max: [f64; 2],
        color: [u8; 4],
        fill: bool,
        width: f32,
    },
    Circle {
        center: [f64; 2],
        radius: f32,
        color: [u8; 4],
        fill: bool,
        width: f32,
    },
    Text {
        point: [f64; 2],
        text: String,
        color: [u8; 4],
    },
}
fn bad(s: &str) -> mlua::Error {
    stop(Category::Runtime, s)
}
pub(crate) fn fields(t: &Table, allowed: &[&str]) -> mlua::Result<()> {
    if t.metatable().is_some() {
        return Err(bad("UI descriptors must be plain tables."));
    }
    for p in t.clone().pairs::<Value, Value>() {
        let (k, _) = p?;
        if !matches!(k,Value::String(ref s) if s.as_bytes().len()<=16 && allowed.contains(&s.to_str()?.as_ref()))
        {
            return Err(bad("Unknown UI descriptor field."));
        }
    }
    Ok(())
}
fn string(v: Value, default: Option<&str>, max: usize) -> mlua::Result<String> {
    match v {
        Value::Nil if default.is_some() => Ok(default.unwrap().into()),
        Value::String(s) if s.as_bytes().len() <= max => Ok(s.to_str()?.to_string()),
        _ => Err(bad("UI text must be bounded UTF-8 strings.")),
    }
}
fn number(v: Value, default: Option<f64>, min: f64, max: f64) -> mlua::Result<f64> {
    let n = match v {
        Value::Nil if default.is_some() => default.unwrap(),
        Value::Integer(n) => n as f64,
        Value::Number(n) => n,
        _ => return Err(bad("UI values must be finite numbers.")),
    };
    if !n.is_finite() || n < min || n > max {
        return Err(bad("UI numeric value is outside its finite bounds."));
    }
    Ok(n)
}
fn boolean(v: Value, default: bool) -> mlua::Result<bool> {
    match v {
        Value::Nil => Ok(default),
        Value::Boolean(v) => Ok(v),
        _ => Err(bad("UI flags must be booleans.")),
    }
}
fn selection(t: &Table, n: usize) -> mlua::Result<usize> {
    match t.raw_get::<Value>("value")? {
        Value::Nil => Ok(0),
        Value::Integer(i) if i >= 1 && i as usize <= n => Ok(i as usize - 1),
        _ => Err(bad(
            "UI selection value must be an in-range1-based integer.",
        )),
    }
}
fn array(t: &Table, max: usize) -> mlua::Result<usize> {
    let n = t.raw_len();
    if n > max {
        return Err(stop(Category::Budget, "UI array exceeds its finite limit."));
    }
    if t.metatable().is_some() {
        return Err(bad("UI arrays must be plain and dense."));
    }
    let mut count = 0;
    for p in t.clone().pairs::<Value, Value>() {
        let (k, _) = p?;
        count += 1;
        if count > n || !matches!(k,Value::Integer(i) if i>=1 && i as usize<=n) {
            return Err(bad("UI arrays must be plain and dense."));
        }
    }
    if count != n {
        return Err(bad("UI arrays cannot contain holes."));
    }
    Ok(n)
}
struct Parser<'a> {
    lua: &'a Lua,
    ids: BTreeSet<String>,
    primitives: usize,
    points: usize,
    text: usize,
    options: usize,
    tree_items: usize,
}
impl Parser<'_> {
    fn new(lua: &Lua) -> Parser<'_> {
        Parser {
            lua,
            ids: BTreeSet::new(),
            primitives: 0,
            points: 0,
            text: 0,
            options: 0,
            tree_items: 0,
        }
    }
    fn check(&self) -> mlua::Result<()> {
        notes::check_budget(self.lua)
    }
    fn text(&mut self, v: Value, default: Option<&str>, max: usize) -> mlua::Result<String> {
        let s = string(v, default, max)?;
        self.text += s.len();
        if self.text > MAX_TEXT {
            return Err(stop(Category::Budget, "UI snapshot text exceeds64KiB."));
        }
        Ok(s)
    }
    fn choices(&mut self, t: &Table, max: usize) -> mlua::Result<Vec<String>> {
        let n = array(t, max)?;
        if n == 0 {
            return Err(bad("UI selection needs at least one option."));
        }
        self.options += n;
        if self.options > MAX_OPTIONS {
            return Err(stop(
                Category::Budget,
                "UI snapshot exceeds32768selection options.",
            ));
        }
        let mut values = Vec::with_capacity(n);
        for i in 1..=n {
            if i % 64 == 0 {
                self.check()?;
            }
            values.push(self.text(t.raw_get(i)?, None, 256)?);
        }
        Ok(values)
    }
    fn tree(
        &mut self,
        t: Table,
        depth: usize,
        ids: &mut BTreeSet<String>,
    ) -> mlua::Result<Vec<TreeItem>> {
        let n = array(&t, MAX_TREE_ITEMS)?;
        if depth > 8 && n != 0 {
            return Err(stop(Category::Budget, "UI tree exceeds depth8."));
        }
        self.tree_items += n;
        if self.tree_items > MAX_TREE_ITEMS {
            return Err(stop(Category::Budget, "UI snapshot exceeds512tree items."));
        }
        let mut items = Vec::with_capacity(n);
        for i in 1..=n {
            self.check()?;
            let t = t.raw_get::<Table>(i)?;
            fields(
                &t,
                &["id", "label", "tooltip", "enabled", "expanded", "children"],
            )?;
            let id = string(t.raw_get("id")?, None, 64)?;
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                || !ids.insert(id.clone())
            {
                return Err(bad(
                    "Tree items need unique local ASCII ids of at most64bytes.",
                ));
            }
            let label = self.text(t.raw_get("label")?, None, 256)?;
            if label.is_empty() {
                return Err(bad("Tree item labels cannot be empty."));
            }
            let children = match t.raw_get::<Value>("children")? {
                Value::Nil => Vec::new(),
                Value::Table(c) => self.tree(c, depth + 1, ids)?,
                _ => return Err(bad("Tree children must be plain dense arrays.")),
            };
            items.push(TreeItem {
                id,
                label,
                tip: self.text(t.raw_get("tooltip")?, Some(""), 1024)?,
                enabled: boolean(t.raw_get("enabled")?, true)?,
                expanded: boolean(t.raw_get("expanded")?, false)?,
                children,
            });
        }
        Ok(items)
    }
    fn point(&self, t: &Table, x: &str, y: &str) -> mlua::Result<[f64; 2]> {
        Ok([
            number(t.raw_get(x)?, None, -16., 16.)?,
            number(t.raw_get(y)?, None, -16., 16.)?,
        ])
    }
    fn cost(&mut self, n: usize) -> mlua::Result<()> {
        self.points += n;
        if self.points > MAX_POINTS {
            return Err(stop(
                Category::Budget,
                "Canvas exceeds8192point-cost units.",
            ));
        }
        Ok(())
    }
    fn color(&self, v: Value) -> mlua::Result<[u8; 4]> {
        let Value::Table(t) = v else {
            return if matches!(v, Value::Nil) {
                Ok([180, 180, 220, 255])
            } else {
                Err(bad("Canvas colors must be dense RGBA byte arrays."))
            };
        };
        if array(&t, 4)? != 4 {
            return Err(bad("Canvas color needs four RGBA bytes."));
        }
        let mut c = [0; 4];
        for (i, c) in c.iter_mut().enumerate() {
            match t.raw_get::<Value>(i + 1)? {
                Value::Integer(n) if (0..=255).contains(&n) => *c = n as u8,
                _ => return Err(bad("Canvas color components must be integer bytes.")),
            }
        }
        Ok(c)
    }
    fn primitives(&mut self, t: Table) -> mlua::Result<Vec<Primitive>> {
        let n = array(&t, MAX_PRIMITIVES)?;
        self.primitives += n;
        if self.primitives > MAX_PRIMITIVES {
            return Err(stop(
                Category::Budget,
                "UI snapshot exceeds2048canvas primitives.",
            ));
        }
        let mut result = Vec::with_capacity(n);
        for i in 1..=n {
            self.check()?;
            let p = t.raw_get::<Table>(i)?;
            let kind = string(p.raw_get("kind")?, None, 16)?;
            let raw_color = p.raw_get("color")?;
            let color = if kind == "image" && matches!(raw_color, Value::Nil) {
                [255; 4]
            } else {
                self.color(raw_color)?
            };
            let width = number(p.raw_get("width")?, Some(1.), 0.25, 16.)? as f32;
            result.push(match kind.as_str() {
                "gradient" => {
                    fields(
                        &p,
                        &[
                            "kind",
                            "x1",
                            "y1",
                            "x2",
                            "y2",
                            "top_left",
                            "top_right",
                            "bottom_left",
                            "bottom_right",
                        ],
                    )?;
                    self.cost(4)?;
                    let min = self.point(&p, "x1", "y1")?;
                    let max = self.point(&p, "x2", "y2")?;
                    if min[0] >= max[0] || min[1] >= max[1] {
                        return Err(bad(
                            "Gradient rectangle must have positive width and height.",
                        ));
                    }
                    let mut colors = [[0; 4]; 4];
                    for (out, key) in colors.iter_mut().zip([
                        "top_left",
                        "top_right",
                        "bottom_left",
                        "bottom_right",
                    ]) {
                        let v = p.raw_get::<Value>(key)?;
                        if matches!(v, Value::Nil) {
                            return Err(bad("Gradient needs four explicit RGBA corner colors."));
                        }
                        *out = self.color(v)?;
                    }
                    Primitive::Gradient { min, max, colors }
                }
                "image" => {
                    fields(
                        &p,
                        &[
                            "kind", "image", "x1", "y1", "x2", "y2", "u1", "v1", "u2", "v2",
                            "color",
                        ],
                    )?;
                    self.cost(4)?;
                    let min = self.point(&p, "x1", "y1")?;
                    let max = self.point(&p, "x2", "y2")?;
                    let uv_min = [
                        number(p.raw_get("u1")?, Some(0.), 0., 1.)?,
                        number(p.raw_get("v1")?, Some(0.), 0., 1.)?,
                    ];
                    let uv_max = [
                        number(p.raw_get("u2")?, Some(1.), 0., 1.)?,
                        number(p.raw_get("v2")?, Some(1.), 0., 1.)?,
                    ];
                    if min[0] >= max[0]
                        || min[1] >= max[1]
                        || uv_min[0] >= uv_max[0]
                        || uv_min[1] >= uv_max[1]
                    {
                        return Err(bad(
                            "Canvas image and UV rectangles must have positive width and height.",
                        ));
                    }
                    Primitive::Image {
                        resource: images::reference(self.lua, p.raw_get("image")?)?,
                        min,
                        max,
                        uv_min,
                        uv_max,
                        color,
                    }
                }
                "line" => {
                    fields(&p, &["kind", "x1", "y1", "x2", "y2", "color", "width"])?;
                    self.cost(2)?;
                    Primitive::Line {
                        points: vec![self.point(&p, "x1", "y1")?, self.point(&p, "x2", "y2")?],
                        color,
                        width,
                        closed: false,
                    }
                }
                "polyline" => {
                    fields(&p, &["kind", "x", "y", "closed", "color", "width"])?;
                    let x = p.raw_get::<Table>("x")?;
                    let y = p.raw_get::<Table>("y")?;
                    let n = array(&x, MAX_POINTS)?;
                    if n < 2 || array(&y, MAX_POINTS)? != n {
                        return Err(bad(
                            "Canvas polyline needs equal dense x/y columns with at least2points.",
                        ));
                    }
                    self.cost(n)?;
                    let mut points = Vec::with_capacity(n);
                    for j in 1..=n {
                        if j.is_multiple_of(64) {
                            self.check()?;
                        }
                        points.push([
                            number(x.raw_get(j)?, None, -16., 16.)?,
                            number(y.raw_get(j)?, None, -16., 16.)?,
                        ]);
                    }
                    Primitive::Line {
                        points,
                        color,
                        width,
                        closed: boolean(p.raw_get("closed")?, false)?,
                    }
                }
                "rect" => {
                    fields(
                        &p,
                        &["kind", "x1", "y1", "x2", "y2", "color", "width", "fill"],
                    )?;
                    self.cost(4)?;
                    let (min, max) = (self.point(&p, "x1", "y1")?, self.point(&p, "x2", "y2")?);
                    if min[0] > max[0] || min[1] > max[1] {
                        return Err(bad("Canvas rectangle minima must not exceed maxima."));
                    }
                    Primitive::Rect {
                        min,
                        max,
                        color,
                        width,
                        fill: boolean(p.raw_get("fill")?, false)?,
                    }
                }
                "circle" => {
                    fields(&p, &["kind", "x", "y", "radius", "color", "width", "fill"])?;
                    self.cost(64)?;
                    Primitive::Circle {
                        center: self.point(&p, "x", "y")?,
                        radius: number(p.raw_get("radius")?, Some(4.), 0.5, 32.)? as f32,
                        color,
                        width,
                        fill: boolean(p.raw_get("fill")?, false)?,
                    }
                }
                "text" => {
                    fields(&p, &["kind", "x", "y", "text", "color"])?;
                    let text = self.text(p.raw_get("text")?, None, 256)?;
                    self.cost(text.chars().count() * 4)?;
                    Primitive::Text {
                        point: self.point(&p, "x", "y")?,
                        text,
                        color,
                    }
                }
                _ => return Err(bad("Unknown canvas primitive kind.")),
            });
        }
        Ok(result)
    }
    fn node(&mut self, t: Table, depth: usize) -> mlua::Result<Node> {
        self.check()?;
        if t.metatable().is_some() {
            return Err(bad("UI nodes must be plain tables."));
        }
        if depth > 8 || self.ids.len() >= MAX_NODES {
            return Err(stop(Category::Budget, "UI tree exceeds256nodes or depth8."));
        }
        let id = string(t.raw_get("id")?, None, 64)?;
        if id.is_empty()
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
            || !self.ids.insert(id.clone())
        {
            return Err(bad(
                "UI nodes need unique nonempty ASCII ids of at most64bytes.",
            ));
        }
        let name = string(t.raw_get("kind")?, None, 16)?;
        let label = self.text(t.raw_get("label")?, Some(""), 4096)?;
        let tip = self.text(t.raw_get("tooltip")?, Some(""), 4096)?;
        let mut allowed = vec!["id", "kind", "label", "tooltip", "enabled"];
        let kind = match name.as_str() {
            "column" | "row" | "group" | "tabs" => {
                allowed.push("children");
                let children = t.raw_get::<Table>("children")?;
                let n = array(&children, if name == "tabs" { 16 } else { MAX_NODES })?;
                if name == "tabs" && n == 0 {
                    return Err(bad("Tabs need at least one labeled group."));
                }
                let mut nodes = Vec::with_capacity(n);
                for i in 1..=n {
                    nodes.push(self.node(children.raw_get(i)?, depth + 1)?);
                }
                match name.as_str() {
                    "row" => Kind::Row(nodes),
                    "group" => Kind::Group(nodes),
                    "tabs" => {
                        allowed.push("value");
                        if nodes.iter().any(|n| {
                            !matches!(n.kind, Kind::Group(_))
                                || n.label.is_empty()
                                || n.label.len() > 256
                        }) {
                            return Err(bad(
                                "Every tab must be a group with a nonempty label of at most256UTF8bytes.",
                            ));
                        }
                        Kind::Tabs {
                            selected: selection(&t, n)?,
                            children: nodes,
                        }
                    }
                    _ => Kind::Column(nodes),
                }
            }
            "label" => Kind::Label,
            "button" => Kind::Button,
            "toggle" => {
                allowed.push("value");
                Kind::Toggle(boolean(t.raw_get("value")?, false)?)
            }
            "color" => {
                allowed.push("value");
                let value = t.raw_get::<Value>("value")?;
                Kind::Color(if matches!(value, Value::Nil) {
                    [255; 4]
                } else {
                    self.color(value)?
                })
            }
            "tree" => {
                allowed.extend(["items", "value", "height"]);
                let mut ids = BTreeSet::new();
                let items = self.tree(t.raw_get("items")?, 1, &mut ids)?;
                let selected = string(t.raw_get("value")?, Some(""), 64)?;
                if !selected.is_empty() && !ids.contains(&selected) {
                    return Err(bad("Tree selection must be an existing item id or empty."));
                }
                Kind::Tree {
                    items,
                    selected,
                    height: number(t.raw_get("height")?, Some(200.), 64., 1024.)? as f32,
                }
            }
            "number" | "slider" => {
                allowed.extend(["value", "min", "max", "step"]);
                let min = number(t.raw_get("min")?, Some(0.), -1e12, 1e12)?;
                let max = number(t.raw_get("max")?, Some(1.), min, 1e12)?;
                let value = number(t.raw_get("value")?, Some(min), min, max)?;
                let step = number(t.raw_get("step")?, Some(0.01), 1e-12, 1e12)?;
                Kind::Number {
                    value,
                    min,
                    max,
                    step,
                    slider: name == "slider",
                }
            }
            "text" => {
                allowed.push("value");
                Kind::Text(self.text(t.raw_get("value")?, Some(""), 4096)?)
            }
            "choice" | "list" => {
                allowed.extend(["choices", "value"]);
                let choices = t.raw_get::<Table>("choices")?;
                let choices = self.choices(&choices, if name == "list" { 512 } else { 128 })?;
                let selected = selection(&t, choices.len())?;
                if name == "list" {
                    allowed.push("height");
                    Kind::List {
                        choices,
                        selected,
                        height: number(t.raw_get("height")?, Some(160.), 64., 1024.)? as f32,
                    }
                } else {
                    Kind::Choice { choices, selected }
                }
            }
            "canvas" => {
                allowed.extend(["height", "interactive", "primitives"]);
                Kind::Canvas {
                    height: number(t.raw_get("height")?, Some(320.), 64., 1024.)? as f32,
                    interactive: boolean(t.raw_get("interactive")?, true)?,
                    primitives: self.primitives(t.raw_get("primitives")?)?,
                }
            }
            _ => return Err(bad("Unknown UI node kind.")),
        };
        fields(&t, &allowed)?;
        let enabled = boolean(t.raw_get("enabled")?, true)?;
        Ok(Node {
            enabled,
            id,
            label,
            tip,
            kind,
        })
    }
}
pub(super) fn parse(lua: &Lua, t: Table) -> mlua::Result<Snapshot> {
    notes::check_budget(lua)?;
    fields(&t, &["title", "width", "height", "content"])?;
    let allocation = lua
        .app_data_ref::<NativeBudget>()
        .ok_or_else(|| bad("UI accounting unavailable."))?
        .reserve(SNAPSHOT_BYTES)
        .map_err(|e| e.lua())?;
    let mut parser = Parser::new(lua);
    let title = parser.text(t.raw_get("title")?, Some("Plugin panel"), 128)?;
    let width = number(t.raw_get("width")?, Some(640.), 280., 2048.)? as f32;
    let height = number(t.raw_get("height")?, Some(480.), 160., 1536.)? as f32;
    let root = parser.node(t.raw_get("content")?, 1)?;
    parser.check()?;
    Ok(Snapshot {
        revision: 0,
        title,
        width,
        height,
        root,
        text_bytes: parser.text,
        _allocation: allocation,
    })
}
