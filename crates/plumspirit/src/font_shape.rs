//! Host-owned one-shot font shaping. No GUI, configuration or Lua runs in the helper.
use harfrust::{Direction, Feature, Language, Script, ShapeOptions, ShaperData, UnicodeBuffer};
use skrifa::{
    FontRef, MetadataProvider,
    instance::{LocationRef, Size},
};
use std::io::{Read, Write};
use unicode_segmentation::UnicodeSegmentation;

#[path = "font_shape_process.rs"]
mod process;
pub(crate) use process::{Error, run};
pub(crate) const MEMORY_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const MAX_REPLY: usize = 128 * 1024;
pub(crate) const RESERVATION_BYTES: usize = MEMORY_BYTES + MAX_REPLY * 3 + 32768;
pub(crate) const MAX_MILLIS: u64 = 1500;
const MAX_FONT: usize = 64 * 1024 * 1024;
const MAX_HEADER: usize = 16384;
const REQUEST: &[u8; 8] = b"PSFONT02";
const RESPONSE: &[u8; 8] = b"PSHAPE02";
const FAILURE: &[u8; 8] = b"PSHERR02";
const FLAG: &str = "--internal-font-shape-v1";

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub face: u32,
    pub text: String,
    pub direction: Option<String>,
    pub script: Option<String>,
    pub language: Option<String>,
    pub features: Vec<(String, u32)>,
    pub cap: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Glyph {
    pub id: u32,
    pub cluster: u32,
    pub xa: i32,
    pub ya: i32,
    pub xo: i32,
    pub yo: i32,
    pub unsafe_break: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub direction: String,
    pub script: String,
    pub language: Option<String>,
    pub glyphs: Vec<Glyph>,
}
struct Options {
    direction: Option<Direction>,
    script: Option<Script>,
    language: Option<Language>,
    features: Vec<Feature>,
}
impl Request {
    fn options(&self) -> Result<Options, String> {
        if self.face >= 256
            || self.cap == 0
            || self.cap > 4096
            || self.text.len() > 4096
            || self.text.chars().count() > 256
            || self.text.contains(['\r', '\n', '\u{2028}', '\u{2029}'])
        {
            return Err("Invalid bounded font shaping request.".into());
        }
        let direction = match self.direction.as_deref() {
            None | Some("auto") => None,
            Some("ltr") => Some(Direction::LeftToRight),
            Some("rtl") => Some(Direction::RightToLeft),
            Some("ttb") => Some(Direction::TopToBottom),
            Some("btt") => Some(Direction::BottomToTop),
            _ => return Err("Invalid font direction.".into()),
        };
        let script = if let Some(s) = &self.script {
            if s.len() != 4 || !s.bytes().all(|c| c.is_ascii_alphabetic()) {
                return Err("Invalid script tag.".into());
            }
            Some(s.parse().map_err(|_| "Invalid script tag.")?)
        } else {
            None
        };
        let language = if let Some(s) = &self.language {
            if s.is_empty()
                || s.len() > 63
                || !s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            {
                return Err("Invalid language tag.".into());
            }
            Some(s.parse().map_err(|_| "Invalid language tag.")?)
        } else {
            None
        };
        if self.features.len() > 16 {
            return Err("Too many font features.".into());
        }
        let mut features = Vec::with_capacity(self.features.len());
        let mut tags = std::collections::BTreeSet::new();
        for (tag, value) in &self.features {
            if tag.len() != 4
                || !tag.bytes().all(|c| c.is_ascii_alphanumeric() || c == b' ')
                || *value > 65535
                || !tags.insert(tag)
            {
                return Err("Invalid font feature.".into());
            }
            features.push(Feature {
                tag: harfrust::Tag::new(tag.as_bytes().try_into().unwrap()),
                value: *value,
                start: 0,
                end: u32::MAX,
            });
        }
        Ok(Options {
            direction,
            script,
            language,
            features,
        })
    }
    fn header(&self, font_len: usize) -> Result<Vec<u8>, String> {
        self.options()?;
        if font_len == 0 || font_len > MAX_FONT {
            return Err("Font input must contain1byte–64MiB.".into());
        }
        let json = serde_json::json!({"version":1,"font_bytes":font_len,"face":self.face,"text":self.text,"direction":self.direction,"script":self.script,"language":self.language,"features":self.features,"max_glyphs":self.cap});
        let json = serde_json::to_vec(&json).map_err(|_| "Cannot encode font request.")?;
        if json.len() > MAX_HEADER {
            return Err("Font request header exceeds16KiB.".into());
        }
        Ok(json)
    }
}
fn u32_at(data: &[u8], offset: &mut usize) -> Result<u32, String> {
    let end = offset.checked_add(4).ok_or("Invalid font reply offset.")?;
    let v = u32::from_le_bytes(
        data.get(*offset..end)
            .ok_or("Truncated font reply.")?
            .try_into()
            .unwrap(),
    );
    *offset = end;
    Ok(v)
}
fn read_request(input: &mut impl Read) -> Result<(Request, Vec<u8>), String> {
    let mut magic = [0u8; 8];
    input
        .read_exact(&mut magic)
        .map_err(|_| "Incomplete font request.")?;
    if &magic != REQUEST {
        return Err("Invalid font protocol version.".into());
    }
    let mut len = [0u8; 4];
    input
        .read_exact(&mut len)
        .map_err(|_| "Incomplete font request header.")?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_HEADER {
        return Err("Invalid font header length.".into());
    }
    let mut header = vec![0; len];
    input
        .read_exact(&mut header)
        .map_err(|_| "Incomplete font header.")?;
    let json: serde_json::Value =
        serde_json::from_slice(&header).map_err(|_| "Invalid font header JSON.")?;
    let o = json.as_object().ok_or("Invalid font header object.")?;
    if o.len() != 9
        || !o.keys().all(|k| {
            [
                "version",
                "font_bytes",
                "face",
                "text",
                "direction",
                "script",
                "language",
                "features",
                "max_glyphs",
            ]
            .contains(&k.as_str())
        })
    {
        return Err("Unknown font request fields.".into());
    }
    let number = |k: &str| {
        o.get(k)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| format!("Invalid font header {k}."))
    };
    if number("version")? != 1 {
        return Err("Unsupported font header version.".into());
    }
    let length = usize::try_from(number("font_bytes")?).map_err(|_| "Invalid font size.")?;
    if length == 0 || length > MAX_FONT {
        return Err("Font input exceeds64MiB.".into());
    }
    let optional = |k: &str| match o.get(k) {
        Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
        _ => Err(format!("Invalid font header {k}.")),
    };
    let mut features = Vec::new();
    let values = o
        .get("features")
        .and_then(|v| v.as_array())
        .ok_or("Invalid font features.")?;
    if values.len() > 16 {
        return Err("Too many font features.".into());
    }
    for v in values {
        let p = v
            .as_array()
            .filter(|p| p.len() == 2)
            .ok_or("Invalid font feature pair.")?;
        features.push((
            p[0].as_str().ok_or("Invalid font feature tag.")?.to_owned(),
            u32::try_from(p[1].as_u64().ok_or("Invalid font feature value.")?)
                .map_err(|_| "Invalid font feature value.")?,
        ));
    }
    let request = Request {
        face: u32::try_from(number("face")?).map_err(|_| "Invalid font face.")?,
        text: o
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or("Invalid font text.")?
            .to_owned(),
        direction: optional("direction")?,
        script: optional("script")?,
        language: optional("language")?,
        features,
        cap: usize::try_from(number("max_glyphs")?).map_err(|_| "Invalid font cap.")?,
    };
    request.options()?;
    let mut font = Vec::new();
    font.try_reserve_exact(length)
        .map_err(|_| "Cannot allocate bounded font input.")?;
    font.resize(length, 0);
    input
        .read_exact(&mut font)
        .map_err(|_| "Incomplete font body.")?;
    let mut extra = [0u8; 1];
    if input
        .read(&mut extra)
        .map_err(|_| "Cannot finish font request.")?
        != 0
    {
        return Err("Trailing font request data.".into());
    }
    Ok((request, font))
}
fn direction_name(d: Direction) -> &'static str {
    match d {
        Direction::RightToLeft => "rtl",
        Direction::TopToBottom => "ttb",
        Direction::BottomToTop => "btt",
        _ => "ltr",
    }
}
pub(crate) fn shape(request: &Request, font: &[u8]) -> Result<Run, String> {
    let opts = request.options()?;
    let face = FontRef::from_index(font, request.face)
        .map_err(|_| "Malformed or unsupported font face.")?;
    let metrics = face.metrics(Size::unscaled(), LocationRef::default());
    if metrics.units_per_em == 0 || metrics.glyph_count == 0 {
        return Err("Font has no usable glyph metrics.".into());
    }
    let mut buffer = UnicodeBuffer::new();
    if !buffer.reserve(request.text.chars().count()) {
        return Err("Cannot allocate shaping buffer.".into());
    }
    for (first, grapheme) in request.text.grapheme_indices(true) {
        for ch in grapheme.chars() {
            buffer.add(ch, first as u32);
        }
    }
    if let Some(d) = opts.direction {
        buffer.set_direction(d);
    }
    if let Some(s) = opts.script {
        buffer.set_script(s);
    }
    if let Some(l) = opts.language {
        buffer.set_language(l);
    }
    buffer.set_cluster_level(harfrust::BufferClusterLevel::MonotoneGraphemes);
    buffer.set_flags(harfrust::BufferFlags::BEGINNING_OF_TEXT | harfrust::BufferFlags::END_OF_TEXT);
    buffer.guess_segment_properties();
    let direction = direction_name(buffer.direction()).to_owned();
    let script = buffer.script().tag().to_string();
    let language = buffer.language().map(|l| l.as_str().to_owned());
    let native = ShaperData::new(&face);
    let shaped = native
        .shaper(&face)
        .build()
        .shape(buffer, ShapeOptions::new().features(&opts.features));
    if !shaped.successful() {
        return Err("Native font shaping exhausted its operation or buffer limits; no partial run returned.".into());
    }
    if shaped.len() > request.cap {
        return Err("Shaped output exceeds max_glyphs.".into());
    }
    let mut glyphs = Vec::new();
    glyphs
        .try_reserve_exact(shaped.len())
        .map_err(|_| "Cannot allocate bounded shaped output.")?;
    for (i, p) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        if i.glyph_id >= u32::from(metrics.glyph_count)
            || i.cluster as usize >= request.text.len()
            || !request.text.is_char_boundary(i.cluster as usize)
        {
            return Err("Native font glyph or cluster is invalid.".into());
        }
        glyphs.push(Glyph {
            id: i.glyph_id,
            cluster: i.cluster,
            xa: p.x_advance,
            ya: p.y_advance,
            xo: p.x_offset,
            yo: p.y_offset,
            unsafe_break: i.unsafe_to_break(),
        });
    }
    Ok(Run {
        direction,
        script,
        language,
        glyphs,
    })
}
fn encode(run: &Run) -> Result<Vec<u8>, String> {
    if run.glyphs.len() > 4096
        || run.script.len() != 4
        || run.language.as_ref().is_some_and(|l| l.len() > 63)
    {
        return Err("Invalid native reply metadata.".into());
    }
    let dir = match run.direction.as_str() {
        "ltr" => 1,
        "rtl" => 2,
        "ttb" => 3,
        "btt" => 4,
        _ => return Err("Invalid native direction.".into()),
    };
    let mut out = Vec::new();
    out.try_reserve_exact(80 + run.glyphs.len() * 28)
        .map_err(|_| "Cannot allocate font reply.")?;
    out.extend_from_slice(RESPONSE);
    out.push(dir);
    out.extend_from_slice(run.script.as_bytes());
    if let Some(l) = &run.language {
        out.push(l.len() as u8);
        out.extend_from_slice(l.as_bytes());
    } else {
        out.push(255);
    }
    out.extend_from_slice(&(run.glyphs.len() as u32).to_le_bytes());
    for g in &run.glyphs {
        for v in [
            g.id,
            g.cluster,
            g.xa as u32,
            g.ya as u32,
            g.xo as u32,
            g.yo as u32,
            u32::from(g.unsafe_break),
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    if out.len() > MAX_REPLY {
        return Err("Native font reply exceeds128KiB.".into());
    }
    Ok(out)
}
fn decode(data: &[u8]) -> Result<Run, String> {
    if data.len() > MAX_REPLY {
        return Err("Native font reply exceeds128KiB.".into());
    }
    if data.starts_with(FAILURE) {
        let mut pos = 8;
        let n = u32_at(data, &mut pos)? as usize;
        if n > 1024 || data.len() != pos + n {
            return Err("Invalid native font error reply.".into());
        }
        return Err(std::str::from_utf8(&data[pos..])
            .map_err(|_| "Invalid native font error text.")?
            .to_owned());
    }
    if !data.starts_with(RESPONSE) || data.len() < 18 {
        return Err("Invalid native font reply.".into());
    }
    let direction = match data[8] {
        1 => "ltr",
        2 => "rtl",
        3 => "ttb",
        4 => "btt",
        _ => return Err("Invalid native font direction.".into()),
    }
    .to_owned();
    let script = std::str::from_utf8(&data[9..13])
        .map_err(|_| "Invalid native script.")?
        .to_owned();
    if !script.bytes().all(|c| c.is_ascii_alphabetic()) {
        return Err("Invalid native script tag.".into());
    }
    let mut pos = 14;
    let n = data[13];
    let language = if n == 255 {
        None
    } else {
        if n > 63 {
            return Err("Invalid native language length.".into());
        }
        let end = pos + n as usize;
        let s = std::str::from_utf8(data.get(pos..end).ok_or("Truncated native language.")?)
            .map_err(|_| "Invalid native language.")?;
        pos = end;
        if !s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
            return Err("Invalid native language tag.".into());
        }
        Some(s.to_owned())
    };
    let n = u32_at(data, &mut pos)? as usize;
    if n > 4096 || data.len() != pos + n * 28 {
        return Err("Invalid native font glyph count or reply length.".into());
    }
    let mut glyphs = Vec::new();
    glyphs
        .try_reserve_exact(n)
        .map_err(|_| "Cannot allocate validated font reply.")?;
    for _ in 0..n {
        let id = u32_at(data, &mut pos)?;
        let cluster = u32_at(data, &mut pos)?;
        let xa = u32_at(data, &mut pos)? as i32;
        let ya = u32_at(data, &mut pos)? as i32;
        let xo = u32_at(data, &mut pos)? as i32;
        let yo = u32_at(data, &mut pos)? as i32;
        let flag = u32_at(data, &mut pos)?;
        if flag > 1 {
            return Err("Invalid native font glyph flags.".into());
        }
        glyphs.push(Glyph {
            id,
            cluster,
            xa,
            ya,
            xo,
            yo,
            unsafe_break: flag != 0,
        });
    }
    Ok(Run {
        direction,
        script,
        language,
        glyphs,
    })
}
fn child_main() -> u8 {
    process::prepare_child();
    let result = read_request(&mut std::io::stdin().lock())
        .and_then(|(request, font)| {
            process::verify_fence()?;
            shape(&request, &font)
        })
        .and_then(|run| encode(&run));
    let out = match result {
        Ok(out) => out,
        Err(e) => {
            let text = e.chars().take(256).collect::<String>();
            let mut out = Vec::with_capacity(12 + text.len());
            out.extend_from_slice(FAILURE);
            out.extend_from_slice(&(text.len() as u32).to_le_bytes());
            out.extend_from_slice(text.as_bytes());
            out
        }
    };
    if std::io::stdout().lock().write_all(&out).is_ok() {
        0
    } else {
        2
    }
}
pub(crate) fn entry() -> Option<std::process::ExitCode> {
    let args = std::env::args_os().collect::<Vec<_>>();
    (args.len() == 2 && args[1] == FLAG).then(|| std::process::ExitCode::from(child_main()))
}

#[cfg(test)]
#[path = "font_shape_tests.rs"]
mod tests;
