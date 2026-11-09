//! Compact immutable coverage masks; no RGBA duplication or per-pixel Lua table.
use super::*;

pub(super) const MAX_REGIONS: u32 = 4096;
const HANDLE_BYTES: usize = 512;

#[path = "image_regions.rs"]
mod regions;

pub(super) struct StoredMask {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    _allocation: Reservation,
}

struct LuaMask {
    job: std::sync::Weak<JobLease>,
    data: Rc<RefCell<Option<StoredMask>>>,
    // The pixel buffer expires eagerly; a Lua-retained native cell is still
    // charged until collected, including after close or a failed job.
    _handle: Reservation,
}
const _: () = assert!(
    HANDLE_BYTES
        >= std::mem::size_of::<JobLease>()
            + std::mem::size_of::<RefCell<Option<StoredMask>>>()
            + 64
);

fn store(
    lua: &Lua,
    job: Arc<JobLease>,
    mask: StoredMask,
    handle: Reservation,
) -> mlua::Result<LuaMask> {
    check_scope(lua, &job, "mask")?;
    let data = Rc::new(RefCell::new(Some(mask)));
    lua.app_data_mut::<ImageSlot>()
        .ok_or_else(|| bad("No image scope is active."))?
        .masks
        .push(Rc::downgrade(&data));
    Ok(LuaMask {
        job: Arc::downgrade(&job),
        data,
        _handle: handle,
    })
}

fn span_pixels(
    count: usize,
    width: u32,
    spans: &[crate::lua::geometry::RasterSpan],
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Vec<u8>> {
    check()?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(count)
        .map_err(|_| stop(Category::Memory, "Cannot allocate this A8 mask."))?;
    // Avoid one uninterrupted 16 MiB zero-fill. Each span is at most16384
    // bytes, so checking every span also bounds coverage-fill work.
    while pixels.len() < count {
        check()?;
        pixels.resize((pixels.len() + 65536).min(count), 0);
    }
    for span in spans {
        check()?;
        let row = span.row as usize * width as usize;
        pixels[row + span.first as usize..row + span.last as usize].fill(255);
    }
    check()?;
    Ok(pixels)
}

pub(super) fn install(lua: &Lua, images: &Table) -> mlua::Result<()> {
    images.set(
        "mask_from_spans",
        lua.create_function(|lua, (spans, options): (Table, Table)| {
            let (job, budget) = context(lua)?;
            if options.metatable().is_some() {
                return Err(bad("Span mask options must be a plain table."));
            }
            ops::keys(&options, &[b"width", b"height"], "Span mask")?;
            let width = integer(options.raw_get("width")?, im::MAX_SIDE, "Mask width")?;
            let height = integer(options.raw_get("height")?, im::MAX_SIDE, "Mask height")?;
            let count = size(width, height)?;
            let handle = budget.reserve(HANDLE_BYTES).map_err(|e| e.lua())?;
            let allocation = budget.reserve(count).map_err(|e| e.lua())?;
            let (spans, _scratch) =
                crate::lua::geometry::mask_spans(lua, &spans, width, height, &mut || {
                    notes::check_budget(lua)
                })?;
            let pixels = span_pixels(count, width, &spans, || notes::check_budget(lua))?;
            store(
                lua,
                job,
                StoredMask {
                    width,
                    height,
                    pixels,
                    _allocation: allocation,
                },
                handle,
            )
        })?,
    )
}

enum Kind {
    Alpha,
    Luminance,
    Brightness,
    Color([u8; 3], f64),
}
struct Settings {
    kind: Kind,
    threshold: Option<(f64, f64)>,
    invert: bool,
    alpha_min: u8,
    preserve_alpha: bool,
}
impl Settings {
    fn parse(options: &Table) -> mlua::Result<Self> {
        let name = options
            .raw_get::<Option<String>>("kind")?
            .unwrap_or_else(|| "alpha".into());
        let allowed: &[&[u8]] = match name.as_str() {
            "alpha" => &[b"kind", b"min", b"max", b"invert"],
            "luminance" | "brightness" => &[
                b"kind",
                b"min",
                b"max",
                b"invert",
                b"alpha_min",
                b"preserve_alpha",
            ],
            "color" => &[
                b"kind",
                b"color",
                b"tolerance",
                b"invert",
                b"alpha_min",
                b"preserve_alpha",
            ],
            _ => {
                return Err(bad(
                    "Image mask kind must be alpha, luminance, brightness or color.",
                ));
            }
        };
        ops::keys(options, allowed, "Image mask")?;
        let kind = match name.as_str() {
            "alpha" => Kind::Alpha,
            "luminance" => Kind::Luminance,
            "brightness" => Kind::Brightness,
            _ => Kind::Color(
                color::color_table(&options.raw_get::<Table>("color")?)?,
                color::unit(options, "tolerance", 0.0)?,
            ),
        };
        let threshold = if !options.raw_get::<Value>("min")?.is_nil()
            || !options.raw_get::<Value>("max")?.is_nil()
        {
            let low = color::unit(options, "min", 0.0)?;
            let high = color::unit(options, "max", 1.0)?;
            if low > high {
                return Err(bad("Mask minimum must not exceed maximum."));
            }
            Some((low, high))
        } else {
            None
        };
        let alpha_min = integer(
            options
                .raw_get::<Option<Value>>("alpha_min")?
                .unwrap_or(Value::Integer(1)),
            255,
            "Mask alpha minimum",
        )? as u8;
        let preserve_alpha = if options.raw_get::<Value>("preserve_alpha")?.is_nil() {
            true
        } else {
            ops::flag(options, "preserve_alpha")?
        };
        Ok(Self {
            kind,
            threshold,
            invert: ops::flag(options, "invert")?,
            alpha_min,
            preserve_alpha,
        })
    }

    fn coverage(&self, p: [u8; 4]) -> u8 {
        let rgb = [p[0], p[1], p[2]];
        let mut value = match self.kind {
            Kind::Alpha => f64::from(p[3]) / 255.0,
            Kind::Luminance => color::luminance(rgb),
            Kind::Brightness => color::brightness(rgb),
            Kind::Color(key, tolerance) => {
                if color::distance(rgb, key) <= tolerance {
                    1.0
                } else {
                    0.0
                }
            }
        };
        if let Some((low, high)) = self.threshold {
            value = if (low..=high).contains(&value) {
                1.0
            } else {
                0.0
            };
        }
        if self.invert {
            value = 1.0 - value;
        }
        if !matches!(self.kind, Kind::Alpha) {
            if p[3] < self.alpha_min {
                return 0;
            }
            if self.preserve_alpha {
                value *= f64::from(p[3]) / 255.0;
            }
        }
        (value * 255.0).round().clamp(0.0, 255.0) as u8
    }

    fn pixels(
        &self,
        image: &Rgba,
        mut check: impl FnMut() -> mlua::Result<()>,
    ) -> mlua::Result<Vec<u8>> {
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(image.px.len())
            .map_err(|_| stop(Category::Memory, "Not enough memory for this mask."))?;
        for (i, &pixel) in image.px.iter().enumerate() {
            if i.is_multiple_of(4096) {
                check()?;
            }
            pixels.push(self.coverage(pixel));
        }
        check()?;
        Ok(pixels)
    }
}

pub(super) fn add_image_methods<M: UserDataMethods<LuaImage>>(methods: &mut M) {
    methods.add_method("mask", |lua, this, options: Table| {
        let settings = Settings::parse(&options)?;
        let (job, budget) = context(lua)?;
        this.with(lua, |image| {
            let count = image.rgba.px.len();
            let handle = budget.reserve(HANDLE_BYTES).map_err(|e| e.lua())?;
            let allocation = budget.reserve(count).map_err(|e| e.lua())?;
            let pixels = settings.pixels(&image.rgba, || notes::check_budget(lua))?;
            store(
                lua,
                job,
                StoredMask {
                    width: image.rgba.w,
                    height: image.rgba.h,
                    pixels,
                    _allocation: allocation,
                },
                handle,
            )
        })
    });
}

impl LuaMask {
    fn with<T>(
        &self,
        lua: &Lua,
        operation: impl FnOnce(&StoredMask) -> mlua::Result<T>,
    ) -> mlua::Result<T> {
        let job = self
            .job
            .upgrade()
            .ok_or_else(|| stop(Category::Expired, "This mask belongs to a finished job."))?;
        check_scope(lua, &job, "mask")?;
        let data = self.data.borrow();
        operation(
            data.as_ref()
                .ok_or_else(|| stop(Category::Expired, "This mask is closed."))?,
        )
    }
    fn read(&self, lua: &Lua, x: u32, y: u32, w: u32, h: u32) -> mlua::Result<mlua::LuaString> {
        self.with(lua, |mask| {
            if w == 0
                || h == 0
                || u64::from(x) + u64::from(w) > u64::from(mask.width)
                || u64::from(y) + u64::from(h) > u64::from(mask.height)
            {
                return Err(bad(
                    "The mask rectangle must be nonempty and inside the mask.",
                ));
            }
            let bytes = u64::from(w) * u64::from(h);
            if bytes > MAX_REGION_BYTES as u64 {
                return Err(stop(
                    Category::Budget,
                    "Mask reads are limited to 1 MiB; request smaller rows or tiles.",
                ));
            }
            let budget = lua.app_data_ref::<NativeBudget>().unwrap().clone();
            let _allocation = budget.reserve(bytes as usize).map_err(|e| e.lua())?;
            let mut out = Vec::new();
            out.try_reserve_exact(bytes as usize)
                .map_err(|_| stop(Category::Memory, "Not enough memory for this mask region."))?;
            for row in y..y + h {
                notes::check_budget(lua)?;
                let start = row as usize * mask.width as usize + x as usize;
                out.extend_from_slice(&mask.pixels[start..start + w as usize]);
            }
            lua.create_string(out)
        })
    }
}

impl UserData for LuaMask {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("colorize", |lua, this, color: Table| {
            let color = super::rgba(&color)?;
            let (job, budget) = context(lua)?;
            this.with(lua, |mask| {
                let handle = super::reserve_handle(lua)?;
                let allocation = budget.reserve(mask.pixels.len() * 4).map_err(|e| e.lua())?;
                let pixels = color_pixels(&mask.pixels, color, || notes::check_budget(lua))?;
                super::store_charged(
                    lua,
                    job,
                    Rgba::new(mask.width, mask.height, pixels).map_err(bad)?,
                    "rgba",
                    allocation,
                    handle,
                )
            })
        });
        methods.add_method("note_sampler", |lua, this, options: Table| {
            this.with(lua, |mask| {
                super::note_masks::create(
                    lua,
                    this.job.upgrade().ok_or_else(|| {
                        stop(Category::Expired, "This mask belongs to a finished job.")
                    })?,
                    mask.width,
                    mask.height,
                    &mask.pixels,
                    options,
                )
            })
        });
        methods.add_method("regions", regions::read);
        methods.add_method("info", |lua, this, ()| {
            this.with(lua, |mask| {
                let info = lua.create_table()?;
                info.set("width", mask.width)?;
                info.set("height", mask.height)?;
                info.set("bytes", mask.pixels.len())?;
                info.set("format", "a8")?;
                info.set("origin", "top_left")?;
                Ok(info)
            })
        });
        methods.add_method("pixel", |lua, this, (x, y): (Value, Value)| {
            this.with(lua, |mask| {
                let x = integer(x, mask.width - 1, "Mask X")?;
                let y = integer(y, mask.height - 1, "Mask Y")?;
                Ok(mask.pixels[y as usize * mask.width as usize + x as usize])
            })
        });
        methods.add_method("read", |lua, this, options: Table| {
            ops::keys(&options, &[b"x", b"y", b"width", b"height"], "Mask read")?;
            let (x, y, w, h) = this.with(lua, |mask| {
                color::rectangle(&options, mask.width, mask.height)
            })?;
            this.read(lua, x, y, w, h)
        });
        methods.add_method(
            "row",
            |lua, this, (y, x, count): (Value, Option<Value>, Option<Value>)| {
                let (x, y, count) = this.with(lua, |mask| {
                    let x = integer(x.unwrap_or(Value::Integer(0)), mask.width - 1, "Mask row X")?;
                    let y = integer(y, mask.height - 1, "Mask row Y")?;
                    let count = integer(
                        count.unwrap_or(Value::Integer(i64::from(mask.width - x))),
                        mask.width - x,
                        "Mask row count",
                    )?;
                    Ok((x, y, count))
                })?;
                this.read(lua, x, y, count, 1)
            },
        );
        methods.add_method("bounds", |lua, this, threshold: Option<Value>| {
            let threshold = integer(
                threshold.unwrap_or(Value::Integer(1)),
                255,
                "Mask bounds threshold",
            )? as u8;
            if threshold == 0 {
                return Err(bad("Mask bounds threshold must be 1..255."));
            }
            this.with(lua, |mask| {
                let (mut left, mut top, mut right, mut bottom) = (mask.width, mask.height, 0, 0);
                let mut count = 0u32;
                for (i, &value) in mask.pixels.iter().enumerate() {
                    if i.is_multiple_of(4096) {
                        notes::check_budget(lua)?;
                    }
                    if value < threshold {
                        continue;
                    }
                    let x = (i % mask.width as usize) as u32;
                    let y = (i / mask.width as usize) as u32;
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x);
                    bottom = bottom.max(y);
                    count += 1;
                }
                notes::check_budget(lua)?;
                if count == 0 {
                    return Ok(None);
                }
                let bounds = lua.create_table()?;
                bounds.set("x", left)?;
                bounds.set("y", top)?;
                bounds.set("width", right - left + 1)?;
                bounds.set("height", bottom - top + 1)?;
                bounds.set("count", count)?;
                Ok(Some(bounds))
            })
        });
        methods.add_method("close", |_, this, ()| {
            this.data.borrow_mut().take();
            Ok(())
        });
    }
}

fn color_pixels(
    coverage: &[u8],
    color: [u8; 4],
    mut check: impl FnMut() -> mlua::Result<()>,
) -> mlua::Result<Vec<[u8; 4]>> {
    check()?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(coverage.len())
        .map_err(|_| stop(Category::Memory, "Cannot allocate colorized mask pixels."))?;
    for (i, &coverage) in coverage.iter().enumerate() {
        if i.is_multiple_of(4096) {
            check()?;
        }
        let alpha = (u32::from(coverage) * u32::from(color[3]) + 127) / 255;
        pixels.push([color[0], color[1], color[2], alpha as u8]);
    }
    check()?;
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mask_sampling_stops_inside_native_work() {
        let image = Rgba::new(256, 256, vec![[255, 255, 255, 255]; 256 * 256]).unwrap();
        let settings = Settings {
            kind: Kind::Luminance,
            threshold: None,
            invert: false,
            alpha_min: 1,
            preserve_alpha: true,
        };
        let mut checks = 0;
        let error = settings
            .pixels(&image, || {
                checks += 1;
                if checks == 3 {
                    Err(stop(Category::Canceled, "controlled native cancellation"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(checks, 3);
        assert!(super::super::super::error_text(&error).contains("controlled native cancellation"));
        assert_eq!(image.at(0, 0), [255, 255, 255, 255]);
    }
}

#[cfg(test)]
#[path = "image_span_mask_tests.rs"]
mod span_tests;

#[cfg(test)]
#[path = "image_mask_color_tests.rs"]
mod color_tests;
