use super::*;
use crate::lua::{Capability, Host, Limits, Plain, ScriptError};
use image::{DynamicImage, ImageFormat, RgbaImage};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[path = "image_bigtiff_fixture.rs"]
mod bigtiff;

#[path = "image_analysis_tests.rs"]
mod analysis;

#[path = "image_mapping_tests.rs"]
mod mapping;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "plum-api02-image-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn encode(format: ImageFormat) -> Vec<u8> {
    let image = DynamicImage::ImageRgba8(
        RgbaImage::from_raw(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 10, 20, 30, 255,
            ],
        )
        .unwrap(),
    );
    let image = if format == ImageFormat::Jpeg {
        DynamicImage::ImageRgb8(image.to_rgb8())
    } else {
        image
    };
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, format).unwrap();
    out.into_inner()
}

fn run(path: &Path, source: &str) -> Result<Plain, ScriptError> {
    let (client, ui) = super::super::bridge::channel("test.images");
    let source = source.to_owned();
    let worker = std::thread::spawn(move || {
        let host = Host::new(Limits::default(), [Capability::ReadUserFile]).unwrap();
        {
            let mut session = host.lua.app_data_mut::<Session>().unwrap();
            session.client = Some(client);
            session.interactive = true;
        }
        let result = host.run("image-test.lua", &source);
        host.lua.gc_collect().unwrap();
        assert_eq!(
            host.lua.app_data_ref::<NativeBudget>().unwrap().used(),
            0,
            "Native image buffers leaked after the job"
        );
        result
    });
    ui.recv_timeout(Duration::from_secs(3))
        .unwrap()
        .respond(None, Some(path.to_owned()))
        .unwrap();
    worker.join().unwrap()
}

#[test]
fn approved_png_yields_exact_pixels_rows_regions_and_restores_file_cursor() {
    let temp = Temp::new();
    let path = temp.path("picture.png");
    std::fs::write(&path, encode(ImageFormat::Png)).unwrap();
    assert_eq!(
        run(
            &path,
            r#"
        local f=plum.files.open(); f:seek(3)
        local i=plum.images.load(f)
        assert(f:info().offset==3)
        local info=i:info()
        assert(info.width==2 and info.height==2 and info.format=='png' and info.bytes==16)
        assert(info.alpha=='straight' and info.origin=='top_left')
        local r,g,b,a=i:pixel(1,0); assert(r==0 and g==255 and b==0 and a==128)
        assert(i:row(0)==string.char(255,0,0,255,0,255,0,128))
        assert(i:row(1,1,1)==string.char(10,20,30,255))
        assert(i:read{x=1,y=0,width=1,height=2}==string.char(0,255,0,128,10,20,30,255))
        f:close(); assert(i:info().width==2)
        assert(not pcall(function()i:pixel(2,0)end))
        assert(not pcall(function()i:pixel(0/0,0)end))
        assert(not pcall(function()i:row(2)end))
        assert(not pcall(function()i:row(0,0,0)end))
        assert(not pcall(function()i:read{width=3,height=1}end))
        assert(not pcall(function()i:read{width=0,height=1}end))
        i:close();i:close();assert(not pcall(function()i:info()end))
        return true
    "#
        )
        .unwrap(),
        Plain::Boolean(true)
    );
}

#[test]
fn all_six_formats_match_the_previous_builtin_pixel_pipeline() {
    let temp = Temp::new();
    for (format, name) in [
        (ImageFormat::Png, "png"),
        (ImageFormat::Jpeg, "jpeg"),
        (ImageFormat::Bmp, "bmp"),
        (ImageFormat::Gif, "gif"),
        (ImageFormat::WebP, "webp"),
        (ImageFormat::Tiff, "tiff"),
    ] {
        let bytes = encode(format);
        let old = image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap()
            .to_rgba8();
        let expected: Vec<u8> = old.into_raw();
        let literal = expected
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let path = temp.path(&format!("test.{name}"));
        std::fs::write(&path, &bytes).unwrap();
        let source = format!(
            "local f=plum.files.open();local i=plum.images.load(f);assert(i:info().format=='{name}');return i:read{{width=2,height=2}}==string.char({literal})"
        );
        assert_eq!(run(&path, &source).unwrap(), Plain::Boolean(true), "{name}");
        let shared = crate::image_decode::decode(
            Cursor::new(bytes),
            None,
            crate::image_decode::BUILTIN,
            |_| Ok(()),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(shared.rgba.px.concat(), expected, "built-in {name}");
    }
}

#[test]
fn generated_rgba_and_color_math_have_defined_alpha_and_ranges() {
    let host = Host::new(Limits::default(), []).unwrap();
    assert_eq!(
        host.run(
            "rgba.lua",
            r#"
        local bytes=string.char(0,0,0,0,255,255,255,127)
        local i=plum.images.from_rgba(2,1,bytes)
        assert(i:row(0)==bytes)
        assert(i:info().format=='rgba')
        assert(plum.images.luminance(0,0,0)==0)
        assert(math.abs(plum.images.luminance(255,255,255)-1)<1e-12)
        assert(math.abs(plum.images.luminance(255,0,0)-0.2126)<1e-12)
        assert(math.abs(plum.images.brightness(255,0,0)-1/3)<1e-12)
        assert(not pcall(function()plum.images.luminance(-1,0,0)end))
        assert(not pcall(function()plum.images.brightness(0/0,0,0)end))
        assert(not pcall(function()plum.images.from_rgba(0,1,'')end))
        assert(not pcall(function()plum.images.from_rgba(1,1,'bad')end))
        assert(not pcall(function()plum.images.load(i)end))
        assert(not pcall(function()plum.images.load('C:/private.png')end))
        return true
    "#
        )
        .unwrap(),
        Plain::Boolean(true)
    );
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn large_region_rejected_but_one_mib_tile_works_with_bounded_lua_memory() {
    let host = Host::new(
        Limits {
            memory: 16 * 1024 * 1024,
            ..Limits::default()
        },
        [],
    )
    .unwrap();
    assert_eq!(
        host.run(
            "tile.lua",
            r#"
        local pixel=string.char(1,2,3,4)
        local i=plum.images.from_rgba(1024,1024,string.rep(pixel,1024*1024))
        assert(not pcall(function()i:read{width=1024,height=1024}end))
        local tile=i:read{x=0,y=0,width=1024,height=256}
        assert(#tile==plum.images.max_region_bytes)
        assert(tile:sub(1,4)==pixel and tile:sub(-4)==pixel)
        local reflected=i:transform{flip_x=true}
        local cropped=reflected:crop{width=1024,height=256}
        local half=i:resize(512,512,'box')
        assert(cropped:row(255):sub(-4)==pixel)
        assert(half:info().bytes==1024*1024 and half:pixel(511,511)==1)
        return #tile
    "#
        )
        .unwrap(),
        Plain::Integer(MAX_REGION_BYTES as i64)
    );
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn retained_images_expire_and_release_memory_after_success_or_error() {
    let host = Host::new(Limits::default(), []).unwrap();
    host.run(
        "one.lua",
        "held=plum.images.from_rgba(1,1,string.char(1,2,3,4))",
    )
    .unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(
        host.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        HANDLE_BYTES
    );
    assert_eq!(
        host.run("expired.lua", "held:pixel(0,0)")
            .unwrap_err()
            .category,
        Category::Expired
    );
    host.run(
        "bad.lua",
        "held=plum.images.from_rgba(1,1,string.char(1,2,3,4));error('failure')",
    )
    .unwrap_err();
    host.lua.gc_collect().unwrap();
    assert_eq!(
        host.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        HANDLE_BYTES
    );
    assert_eq!(
        host.run("expired.lua", "held:info()").unwrap_err().category,
        Category::Expired
    );
    host.lua.globals().set("held", Value::Nil).unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn image_counts_dimensions_and_native_quota_fail_before_pixel_allocation() {
    let host = Host::new(Limits::default(), []).unwrap();
    host.lua.set_app_data(NativeBudget::isolated(
        super::super::bridge::NATIVE_PLUGIN_BYTES,
    ));
    assert_eq!(host.run("count.lua","local images={} for i=1,65 do images[i]=plum.images.from_rgba(1,1,string.char(0,0,0,0)) end").unwrap_err().category,Category::Budget);
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    assert_eq!(
        host.run("huge.lua", "plum.images.from_rgba(4096,4097,'')")
            .unwrap_err()
            .category,
        Category::Budget
    );
    let budget = host.lua.app_data_ref::<NativeBudget>().unwrap().clone();
    let held = budget
        .reserve(super::super::bridge::NATIVE_PLUGIN_BYTES)
        .unwrap();
    assert_eq!(
        host.run(
            "native.lua",
            "plum.images.from_rgba(1,1,string.char(0,0,0,0))"
        )
        .unwrap_err()
        .category,
        Category::Budget
    );
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn immutable_crop_resize_and_transform_have_exact_independent_pixels() {
    let host = Host::new(Limits::default(), []).unwrap();
    assert_eq!(
        host.run(
            "image-ops.lua",
            r#"
        local a=string.char(255,0,0,255)
        local b=string.char(0,255,0,128)
        local c=string.char(0,0,255,0)
        local d=string.char(10,20,30,255)
        local i=plum.images.from_rgba(2,2,a..b..c..d)
        local crop=i:crop{x=1,width=1,height=2}
        assert(crop:read{width=1,height=2}==b..d)
        local identity=i:transform{}
        assert(identity~=i and identity:read{width=2,height=2}==a..b..c..d)
        assert(i:transform{flip_x=true}:read{width=2,height=2}==b..a..d..c)
        assert(i:transform{scale_y=-1}:read{width=2,height=2}==c..d..a..b)
        assert(i:transform{flip_x=true,scale_x=-1}:read{width=2,height=2}==a..b..c..d)
        assert(i:transform{rotate=90}:read{width=2,height=2}==c..a..d..b)
        assert(i:transform{rotate=180}:read{width=2,height=2}==d..c..b..a)
        local doubled=i:transform{scale_x=2}
        assert(doubled:info().width==4 and doubled:row(0)==a..a..b..b)
        local nearest=i:resize(4,2,'nearest')
        assert(nearest:row(0)==a..a..b..b and nearest:row(1)==c..c..d..d)
        -- Pixel-centre nearest mapping chooses bottom-right when reducing 2x2 to 1x1.
        assert(i:resize(1,1,'nearest'):row(0)==d)
        local average=i:resize(1,1)
        -- Box averages each straight RGBA channel, including RGB with alpha zero.
        assert(average:row(0)==string.char(66,69,71,160))
        assert(i:resize(2,2,'box'):read{width=2,height=2}==a..b..c..d)
        assert(i:row(0)==a..b and i:row(1)==c..d)
        i:close()
        assert(crop:row(1)==d and identity:row(0)==a..b)
        held_transform=identity
        return true
    "#
        )
        .unwrap(),
        Plain::Boolean(true)
    );
    host.lua.gc_collect().unwrap();
    assert_eq!(
        host.lua.app_data_ref::<NativeBudget>().unwrap().used(),
        HANDLE_BYTES
    );
    assert_eq!(
        host.run("expired-transform.lua", "held_transform:transform{}")
            .unwrap_err()
            .category,
        Category::Expired
    );
    host.lua
        .globals()
        .set("held_transform", Value::Nil)
        .unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn image_operations_reject_invalid_nonfinite_and_oversized_inputs() {
    let host = Host::new(Limits::default(), []).unwrap();
    host.run(
        "bad-image-ops.lua",
        r#"
        local i=plum.images.from_rgba(2,1,string.char(1,2,3,4,5,6,7,8))
        for _,args in ipairs({
            {width=0,height=1},{width=-1,height=1},{x=2,width=1,height=1},
            {y=1,width=1,height=1},{width=1,height=2},{width=1,height=1,pivot=0},
            {width=1.5,height=1},{x=0/0,width=1,height=1}
        }) do assert(not pcall(function()i:crop(args)end)) end
        for _,args in ipairs({
            {rotate=0/0},{rotate=math.huge},{scale_x=0},{scale_y=0.0001},
            {scale_x=-math.huge},{scale_y=1e308},{flip_x=1},{rotate='90'},
            {pivot_x=0},{scale_x=16385}
        }) do assert(not pcall(function()i:transform(args)end)) end
        for _,args in ipairs({{0,1},{1,-1},{1.5,1},{1,math.huge},{1,1,'cubic'}}) do
            assert(not pcall(function()i:resize(table.unpack(args))end))
        end
        assert(not pcall(function()i:resize(4096,4097)end))
        assert(i:row(0)==string.char(1,2,3,4,5,6,7,8))
    "#,
    )
    .unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
    assert_eq!(
        host.run(
            "huge-transform.lua",
            r#"
        local i=plum.images.from_rgba(2,1,string.char(1,2,3,4,5,6,7,8))
        i:transform{scale_x=2048,scale_y=4097}
    "#
        )
        .unwrap_err()
        .category,
        Category::Budget
    );
    host.lua.gc_collect().unwrap();
    assert_eq!(host.lua.app_data_ref::<NativeBudget>().unwrap().used(), 0);
}

#[test]
fn derived_image_workspace_and_handle_quotas_release_after_failure() {
    let host = Host::new(Limits::default(), []).unwrap();
    let budget = NativeBudget::isolated(64 + 3 * HANDLE_BYTES);
    host.lua.set_app_data(budget.clone());
    let failure = host
        .run(
            "image-workspace.lua",
            r#"
        local i=plum.images.from_rgba(2,2,string.rep(string.char(1,2,3,4),4))
        local crop=i:crop{width=1,height=1}
        local nearest=i:resize(1,1,'nearest')
        -- Pixels fit; the preflighted box span workspace does not.
        assert(not pcall(function()i:resize(1,1,'box')end))
        assert(crop:pixel(0,0)==1 and nearest:pixel(0,0)==1)
        assert(not pcall(function()i:transform{scale_x=4,scale_y=4}end))
        held_derived=crop
        error('deliberate failure after allocations')
    "#,
        )
        .unwrap_err();
    assert!(
        failure
            .message
            .contains("deliberate failure after allocations"),
        "{}",
        failure.message
    );
    host.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), HANDLE_BYTES);
    host.lua.globals().set("held_derived", Value::Nil).unwrap();
    host.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
    let budget = NativeBudget::isolated(64 * HANDLE_BYTES + 8);
    host.lua.set_app_data(budget.clone());
    let err = host
        .run(
            "derived-handles.lua",
            r#"
        local i=plum.images.from_rgba(1,1,string.char(1,2,3,4))
        for n=1,64 do i:transform{}:close() end
    "#,
        )
        .unwrap_err();
    assert_eq!(err.category, Category::Budget);
    assert!(err.message.contains("At most 64 image resources"));
    host.lua.gc_collect().unwrap();
    assert_eq!(budget.used(), 0);
}

fn png_header(width: u32, height: u32) -> Vec<u8> {
    use plumspirit_engine::codec::Crc32;
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = Crc32::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(&crc.finish().to_be_bytes());
    };
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &[0x78, 0x9c, 3, 0, 0, 0, 0, 1]);
    chunk(b"IEND", &[]);
    out
}

#[test]
fn malformed_truncated_and_oversized_images_fail_cleanly() {
    let temp = Temp::new();
    for (name, bytes) in [
        ("bad.png", b"not an image".to_vec()),
        ("cut.png", encode(ImageFormat::Png)[..20].to_vec()),
        ("huge.png", png_header(1_000_000, 1_000_000)),
        ("pixels.png", png_header(4096, 4097)),
    ] {
        let path = temp.path(name);
        std::fs::write(&path, bytes).unwrap();
        let error = run(
            &path,
            "local f=plum.files.open();return plum.images.load(f)",
        )
        .unwrap_err();
        assert_eq!(error.category, Category::Runtime, "{error}");
        assert!(error.message.starts_with("decode:"), "{error}");
    }
    let path = temp.path("large.png");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(MAX_FILE_BYTES + 1)
        .unwrap();
    assert_eq!(
        run(
            &path,
            "local f=plum.files.open();return plum.images.load(f)"
        )
        .unwrap_err()
        .category,
        Category::Budget
    );
}

#[test]
fn animation_uses_first_frame_and_preflight_cancellation_stops_before_pixels() {
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            encoder
                .encode_frame(image::Frame::new(
                    RgbaImage::from_raw(1, 1, color.to_vec()).unwrap(),
                ))
                .unwrap();
        }
    }
    let decoded = crate::image_decode::decode(
        Cursor::new(&bytes),
        None,
        crate::image_decode::BUILTIN,
        |_| Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(decoded.rgba.px, [[255, 0, 0, 255]]);
    let error = crate::image_decode::decode(
        Cursor::new(bytes),
        None,
        crate::image_decode::BUILTIN,
        |_| Err("resource preflight denied".into()),
        || Ok(()),
    )
    .err()
    .unwrap();
    assert!(error.contains("resource preflight denied"));
    let error = crate::image_decode::decode(
        Cursor::new(encode(ImageFormat::Png)),
        None,
        crate::image_decode::BUILTIN,
        |_| Ok(()),
        || Err("canceled".into()),
    )
    .err()
    .unwrap();
    assert!(error.contains("canceled"));
}

#[test]
fn basename_fallback_preserves_existing_bigtiff_support() {
    let temp = Temp::new();
    let path = temp.path("variant.tiff");
    std::fs::write(&path, bigtiff::grayscale_pixel()).unwrap();
    let old = image::ImageReader::open(&path)
        .unwrap()
        .with_guessed_format()
        .unwrap()
        .decode()
        .unwrap()
        .to_rgba8();
    assert_eq!(old.into_raw(), [77, 77, 77, 255]);
    assert_eq!(run(&path,"local f=plum.files.open();local i=plum.images.load(f);local r,g,b,a=i:pixel(0,0);return r==77 and g==77 and b==77 and a==255").unwrap(),Plain::Boolean(true));
}

#[test]
fn high_depth_decode_preflight_counts_codec_and_external_raw_buffers() {
    let temp = Temp::new();
    let path = temp.path("large-float.tiff");
    std::fs::write(&path, bigtiff::rgba_float_header(3000, 3000)).unwrap();
    let error = run(
        &path,
        "local f=plum.files.open();return plum.images.load(f)",
    )
    .unwrap_err();
    assert_eq!(error.category, Category::Budget, "{error}");
    assert!(error.message.contains("native memory quota"), "{error}");
}

#[test]
fn header_budget_does_not_prevent_a_larger_valid_pixel_payload() {
    let temp = Temp::new();
    let path = temp.path("large.png");
    let image = RgbaImage::from_pixel(1024, 1024, image::Rgba([7, 17, 29, 31]));
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    assert!(encoded.get_ref().len() < 1024 * 1024);
    std::fs::write(&path, encoded.into_inner()).unwrap();
    assert_eq!(run(&path,"local f=plum.files.open();local i=plum.images.load(f);local r,g,b,a=i:pixel(1023,1023);return i:info().bytes==4194304 and r==7 and g==17 and b==29 and a==31").unwrap(),Plain::Boolean(true));
}

#[test]
fn sixteen_bit_png_and_tiff_preserve_previous_rgba8_conversion() {
    let temp = Temp::new();
    for (format, name) in [
        (ImageFormat::Png, "deep.png"),
        (ImageFormat::Tiff, "deep.tiff"),
    ] {
        let image = image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::from_raw(
            2,
            1,
            vec![0, 256, 65535, 32768, 1234, 4321, 60000, 65535],
        )
        .unwrap();
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba16(image)
            .write_to(&mut encoded, format)
            .unwrap();
        let data = encoded.into_inner();
        let path = temp.path(name);
        std::fs::write(&path, &data).unwrap();
        let old = image::ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap()
            .to_rgba8()
            .into_raw();
        let literal = old.iter().map(u8::to_string).collect::<Vec<_>>().join(",");
        assert_eq!(run(&path,&format!("local f=plum.files.open();return plum.images.load(f):row(0)==string.char({literal})")).unwrap(),Plain::Boolean(true));
    }
}

#[test]
fn metadata_reads_cannot_reset_their_allowance_by_seeking() {
    let mut cursor = Cursor::new(vec![9u8; 128]);
    let header = Rc::new(Cell::new(true));
    let exhausted = Rc::new(Cell::new(false));
    let mut reader = DecodeSource {
        source: &mut cursor,
        header: Rc::clone(&header),
        remaining: 64,
        exhausted: Rc::clone(&exhausted),
    };
    reader.read_exact(&mut [0; 64]).unwrap();
    reader.seek(SeekFrom::Start(0)).unwrap();
    assert!(reader.read(&mut [0; 1]).is_err());
    assert!(exhausted.get());
    header.set(false);
    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
}
