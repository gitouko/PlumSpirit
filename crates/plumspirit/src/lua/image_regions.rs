//! Bounded connected-component summaries, not an unbounded point/label table.
use super::*;

#[derive(Clone, Copy)]
struct Region {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    count: u32,
}
struct Plan {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    threshold: u8,
    diagonal: bool,
    max: usize,
}
impl Plan {
    fn pixels(&self) -> usize {
        self.width as usize * self.height as usize
    }
    fn workspace(&self) -> usize {
        self.pixels() * std::mem::size_of::<u32>()
            + self.pixels().div_ceil(8)
            + self.max * std::mem::size_of::<Region>()
    }
}
fn buffer<T>(count: usize) -> mlua::Result<Vec<T>> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|_| {
        stop(
            Category::Memory,
            "Not enough memory for connected mask regions.",
        )
    })?;
    Ok(values)
}

fn scan(
    mask: &StoredMask,
    plan: &Plan,
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Vec<Region>> {
    let mut visited = buffer::<u8>(plan.pixels().div_ceil(8))?;
    visited.resize(plan.pixels().div_ceil(8), 0);
    let mut queue = buffer::<u32>(plan.pixels())?;
    let mut out = buffer::<Region>(plan.max)?;
    let selected = |index: usize| {
        let x = plan.x + index as u32 % plan.width;
        let y = plan.y + index as u32 / plan.width;
        mask.pixels[y as usize * mask.width as usize + x as usize] >= plan.threshold
    };
    for first in 0..plan.pixels() {
        if first.is_multiple_of(4096) {
            check()?;
        }
        if visited[first / 8] & (1 << (first % 8)) != 0 || !selected(first) {
            continue;
        }
        if out.len() == plan.max {
            return Err(stop(
                Category::Budget,
                "Connected mask region count exceeded max_regions; reduce the area or raise the limit up to 4096.",
            ));
        }
        queue.clear();
        queue.push(first as u32);
        visited[first / 8] |= 1 << (first % 8);
        let (mut left, mut top, mut right, mut bottom) = (plan.width, plan.height, 0, 0);
        let mut head = 0usize;
        while head < queue.len() {
            if head.is_multiple_of(4096) {
                check()?;
            }
            let index = queue[head];
            head += 1;
            let (x, y) = (index % plan.width, index / plan.width);
            left = left.min(x);
            right = right.max(x);
            top = top.min(y);
            bottom = bottom.max(y);
            for (dx, dy) in [
                (-1, 0),
                (1, 0),
                (0, -1),
                (0, 1),
                (-1, -1),
                (1, -1),
                (-1, 1),
                (1, 1),
            ]
            .into_iter()
            .take(if plan.diagonal { 8 } else { 4 })
            {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= plan.width as i32 || ny >= plan.height as i32 {
                    continue;
                }
                let next = ny as usize * plan.width as usize + nx as usize;
                if visited[next / 8] & (1 << (next % 8)) == 0 && selected(next) {
                    visited[next / 8] |= 1 << (next % 8);
                    queue.push(next as u32);
                }
            }
        }
        out.push(Region {
            x: plan.x + left,
            y: plan.y + top,
            width: right - left + 1,
            height: bottom - top + 1,
            count: queue.len() as u32,
        });
    }
    check()?;
    Ok(out)
}

pub(super) fn read(lua: &Lua, mask: &LuaMask, options: Option<Table>) -> mlua::Result<Table> {
    let options = options.unwrap_or(lua.create_table()?);
    ops::keys(
        &options,
        &[
            b"x",
            b"y",
            b"width",
            b"height",
            b"threshold",
            b"connectivity",
            b"max_regions",
        ],
        "Mask regions",
    )?;
    let threshold = integer(
        options
            .raw_get::<Option<Value>>("threshold")?
            .unwrap_or(Value::Integer(1)),
        255,
        "Region threshold",
    )? as u8;
    let connectivity = integer(
        options
            .raw_get::<Option<Value>>("connectivity")?
            .unwrap_or(Value::Integer(4)),
        8,
        "Region connectivity",
    )?;
    let max = integer(
        options
            .raw_get::<Option<Value>>("max_regions")?
            .unwrap_or(Value::Integer(i64::from(MAX_REGIONS))),
        MAX_REGIONS,
        "Maximum regions",
    )?;
    if threshold == 0 || max == 0 || !matches!(connectivity, 4 | 8) {
        return Err(bad(
            "Region threshold must be 1..255, connectivity 4 or 8, max_regions 1..4096.",
        ));
    }
    mask.with(lua, |mask| {
        let (x, y, width, height) = color::rectangle(&options, mask.width, mask.height)?;
        let plan = Plan {
            x,
            y,
            width,
            height,
            threshold,
            diagonal: connectivity == 8,
            max: max as usize,
        };
        let budget = lua.app_data_ref::<NativeBudget>().unwrap().clone();
        let _workspace = budget.reserve(plan.workspace()).map_err(|e| e.lua())?;
        let regions = scan(mask, &plan, || notes::check_budget(lua))?;
        let out = lua.create_table_with_capacity(regions.len(), 0)?;
        for (i, r) in regions.into_iter().enumerate() {
            if i.is_multiple_of(256) {
                notes::check_budget(lua)?;
            }
            let item = lua.create_table()?;
            item.raw_set("x", r.x)?;
            item.raw_set("y", r.y)?;
            item.raw_set("width", r.width)?;
            item.raw_set("height", r.height)?;
            item.raw_set("count", r.count)?;
            out.raw_set(i + 1, item)?;
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flood_fill_cancels_inside_a_dense_component() {
        let budget = NativeBudget::isolated(1024 * 1024);
        let mask = StoredMask {
            width: 256,
            height: 256,
            pixels: vec![255; 256 * 256],
            _allocation: budget.reserve(256 * 256).unwrap(),
        };
        let plan = Plan {
            x: 0,
            y: 0,
            width: 256,
            height: 256,
            threshold: 1,
            diagonal: true,
            max: 1,
        };
        let _workspace = budget.reserve(plan.workspace()).unwrap();
        let mut checks = 0;
        let error = scan(&mask, &plan, || {
            checks += 1;
            if checks == 3 {
                Err(stop(
                    Category::Canceled,
                    "controlled flood fill cancellation",
                ))
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert_eq!(checks, 3);
        assert!(
            super::super::super::super::error_text(&error)
                .contains("controlled flood fill cancellation")
        );
        assert_eq!(mask.pixels[0], 255);
    }
}
