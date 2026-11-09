//! Immutable image operations sharing the native converter's pixel algorithms.
use super::*;

type Preflight<'a> = dyn FnMut(u32, u32, usize) -> Result<(), String> + 'a;
type Checkpoint<'a> = dyn FnMut() -> Result<(), String> + 'a;

impl LuaImage {
    pub(super) fn derived(
        &self,
        lua: &Lua,
        operation_key: [u64; 12],
        operation: impl FnOnce(&Rgba, &mut Preflight<'_>, &mut Checkpoint<'_>) -> Result<Rgba, String>,
    ) -> mlua::Result<LuaImage> {
        let (job, budget) = context(lua)?;
        self.with(lua, |image| {
            // The registry retains weak allocation identities until end_run,
            // preventing address reuse (ABA) within this cache's job scope.
            let key = CacheKey {
                source: Rc::as_ptr(&self.data) as usize,
                operation: operation_key,
            };
            let cached = {
                let mut slot = lua.app_data_mut::<ImageSlot>().unwrap();
                slot.cache.retain(|(_, value)| value.strong_count() > 0);
                slot.cache
                    .iter()
                    .find(|(candidate, _)| *candidate == key)
                    .and_then(|(_, value)| value.upgrade())
            };
            if let Some(cached) = cached {
                return shared(lua, job, cached);
            }
            let mut allocation = None;
            let mut preflight_error = None;
            let mut checkpoint_error = None;
            let result = operation(
                &image.rgba,
                &mut |w, h, scratch| {
                    let result = size(w, h).and_then(|count| {
                        let bytes = (count * 4).checked_add(scratch).ok_or_else(|| {
                            stop(Category::Budget, "Image workspace size overflowed.")
                        })?;
                        budget.reserve(bytes).map_err(|e| e.lua())
                    });
                    match result {
                        Ok(guard) => {
                            allocation = Some(guard);
                            Ok(())
                        }
                        Err(error) => {
                            preflight_error = Some(error);
                            Err("Image allocation preflight failed.".into())
                        }
                    }
                },
                &mut || match notes::check_budget(lua) {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        checkpoint_error = Some(error);
                        Err("Image operation interrupted.".into())
                    }
                },
            );
            if let Some(error) = checkpoint_error.or(preflight_error) {
                return Err(error);
            }
            notes::check_budget(lua)?;
            let rgba = result.map_err(|error| {
                if error.starts_with("memory:") {
                    stop(Category::Memory, error)
                } else {
                    bad(error)
                }
            })?;
            let result = store(
                lua,
                job,
                rgba,
                "rgba",
                allocation.ok_or_else(|| bad("Image operation omitted allocation preflight."))?,
            )?;
            let value = Rc::downgrade(result.data.borrow().as_ref().unwrap());
            let mut slot = lua.app_data_mut::<ImageSlot>().unwrap();
            if slot.cache.len() == MAX_CACHE_ENTRIES {
                slot.cache.remove(0);
            }
            slot.cache.push((key, value));
            Ok(result)
        })
    }
}

pub(super) fn keys(options: &Table, allowed: &[&[u8]], label: &str) -> mlua::Result<()> {
    for pair in options.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        if !matches!(key, Value::String(s) if allowed.contains(&s.as_bytes().as_ref())) {
            return Err(bad(format!("{label} contains an unknown option.")));
        }
    }
    Ok(())
}

pub(super) fn number(options: &Table, key: &str, default: f64) -> mlua::Result<f64> {
    match options.raw_get::<Value>(key)? {
        Value::Nil => Ok(default),
        Value::Integer(v) => Ok(v as f64),
        Value::Number(v) if v.is_finite() => Ok(v),
        _ => Err(bad(format!("Image {key} must be a finite number."))),
    }
}

pub(super) fn flag(options: &Table, key: &str) -> mlua::Result<bool> {
    match options.raw_get::<Value>(key)? {
        Value::Nil => Ok(false),
        Value::Boolean(v) => Ok(v),
        _ => Err(bad(format!("Image {key} must be a boolean."))),
    }
}

fn pixels(w: u32, h: u32) -> Result<Vec<[u8; 4]>, String> {
    let mut out = Vec::new();
    out.try_reserve_exact(w as usize * h as usize)
        .map_err(|_| "memory: image pixels could not be allocated".to_string())?;
    Ok(out)
}

pub(super) fn add_methods<M: UserDataMethods<LuaImage>>(methods: &mut M) {
    methods.add_method("crop", |lua, this, options: Table| {
        keys(&options, &[b"x", b"y", b"width", b"height"], "Image crop")?;
        let coord = |key, default| {
            integer(
                options.get::<Option<Value>>(key)?.unwrap_or(default),
                im::MAX_SIDE,
                key,
            )
        };
        let x = coord("x", Value::Integer(0))?;
        let y = coord("y", Value::Integer(0))?;
        let w = coord("width", Value::Nil)?;
        let h = coord("height", Value::Nil)?;
        let mut key = [0; 12];
        key[..5].copy_from_slice(&[1, u64::from(x), u64::from(y), u64::from(w), u64::from(h)]);
        this.derived(lua, key, |src, before, check| {
            if w == 0
                || h == 0
                || u64::from(x) + u64::from(w) > u64::from(src.w)
                || u64::from(y) + u64::from(h) > u64::from(src.h)
            {
                return Err("The crop must be nonempty and inside the image.".into());
            }
            before(w, h, 0)?;
            let mut px = pixels(w, h)?;
            for row in y..y + h {
                check()?;
                let start = row as usize * src.w as usize + x as usize;
                px.extend_from_slice(&src.px[start..start + w as usize]);
            }
            Ok(Rgba { w, h, px })
        })
    });
    methods.add_method(
        "resize",
        |lua, this, (w, h, filter): (Value, Value, Option<String>)| {
            let w = integer(w, im::MAX_SIDE, "Resize width")?;
            let h = integer(h, im::MAX_SIDE, "Resize height")?;
            size(w, h)?;
            let filter = filter.as_deref().unwrap_or("box");
            if !matches!(filter, "box" | "nearest") {
                return Err(bad("Image resize filter must be 'box' or 'nearest'."));
            }
            let mut key = [0; 12];
            key[..4].copy_from_slice(&[2, u64::from(w), u64::from(h), u64::from(filter == "box")]);
            this.derived(lua, key, |src, before, check| {
                if filter == "box" {
                    return im::box_resize_controlled(src, w, h, before, check);
                }
                before(w, h, 0)?;
                let mut px = pixels(w, h)?;
                for y in 0..h {
                    check()?;
                    let sy =
                        ((2 * u64::from(y) + 1) * u64::from(src.h) / (2 * u64::from(h))) as u32;
                    for x in 0..w {
                        let sx =
                            ((2 * u64::from(x) + 1) * u64::from(src.w) / (2 * u64::from(w))) as u32;
                        px.push(src.at(sx, sy));
                    }
                }
                Ok(Rgba { w, h, px })
            })
        },
    );
    methods.add_method("transform", |lua, this, options: Table| {
        keys(
            &options,
            &[b"rotate", b"scale_x", b"scale_y", b"flip_x", b"flip_y"],
            "Image transform",
        )?;
        let transform = im::ImageTransform {
            rotate: number(&options, "rotate", 0.0)?,
            scale_x: number(&options, "scale_x", 1.0)?,
            scale_y: number(&options, "scale_y", 1.0)?,
            flip_x: flag(&options, "flip_x")?,
            flip_y: flag(&options, "flip_y")?,
        };
        let mut key = [0; 12];
        key[..6].copy_from_slice(&[
            3,
            transform.rotate.to_bits(),
            transform.scale_x.to_bits(),
            transform.scale_y.to_bits(),
            u64::from(transform.flip_x),
            u64::from(transform.flip_y),
        ]);
        this.derived(lua, key, |src, before, check| {
            im::transform_controlled(src, &transform, before, check)
        })
    });
}
