//! Shared native palette indexing and bounded cancellable run segmentation.
//! Coordinates stay in image cells; callers author their tick/key/color mapping.
use super::{
    MAX_PALETTE, MAX_PIXELS, MAX_TIME_PIXELS, Orientation, Rgba, Settings, SplitFrom, nearest,
};

const CHECK: u64 = 4096;
pub const MAX_BATCH: usize = 65_536;
fn dimensions(width: u32, height: u32, len: usize) -> Result<usize, String> {
    let count = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || width > MAX_TIME_PIXELS
        || height > MAX_TIME_PIXELS
        || count > MAX_PIXELS
        || count != len as u64
    {
        return Err("Palette run dimensions do not match bounded native pixel data.".into());
    }
    usize::try_from(count).map_err(|_| "Palette run size overflows this platform.".into())
}
/// Preflight the complete index map before allocation; no source pixels are copied.
/// Supports the existing converter's wider processed time axis. A plugin consumer
/// must enforce its own smaller decoded/output/native-budget limits as well.
pub fn index_controlled(
    image: &Rgba,
    settings: &Settings,
    mut before: impl FnMut(usize) -> Result<(), String>,
    mut check: impl FnMut() -> Result<(), String>,
) -> Result<Vec<u32>, String> {
    let count = dimensions(image.w, image.h, image.px.len())?;
    if settings.palette.len() > MAX_PALETTE {
        return Err("Palette exceeds1024colors.".into());
    }
    check()?;
    let bytes = count
        .checked_mul(std::mem::size_of::<u32>())
        .ok_or_else(|| "Palette index allocation overflows this platform.".to_string())?;
    before(bytes)?;
    let mut indices = Vec::new();
    indices
        .try_reserve_exact(count)
        .map_err(|_| "memory: Palette index storage could not be allocated.".to_string())?;
    for chunk in image.px.chunks(CHECK as usize) {
        check()?;
        indices.extend(chunk.iter().map(|pixel| {
            if pixel[3] < settings.alpha_min || settings.palette.is_empty() {
                return u32::MAX;
            }
            let color = nearest(&settings.palette, [pixel[0], pixel[1], pixel[2]]);
            if settings.ignored.get(color).copied().unwrap_or(false) {
                u32::MAX
            } else {
                color as u32
            }
        }));
    }
    check()?;
    Ok(indices)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub line: u32,
    pub start: u32,
    pub length: u32,
    pub color: u32,
}
/// Small copyable position/configuration over caller-owned immutable indexed cells.
/// No self-referential allocation, Lua, document root or buffer is owned here.
/// Callers must supply the same immutable indices for every visit.
#[derive(Clone, Copy)]
pub struct Cursor {
    width: u32,
    height: u32,
    orientation: Orientation,
    lines: u32,
    len: u32,
    line: u32,
    time: u32,
    max_len: u32,
    split: SplitFrom,
    pixel_notes: bool,
    inspected: u64,
}
impl Cursor {
    pub fn new(
        indices: &[u32],
        width: u32,
        height: u32,
        settings: &Settings,
    ) -> Result<Self, String> {
        dimensions(width, height, indices.len())?;
        let (lines, len) = match settings.orientation {
            Orientation::PianoRoll => (height, width),
            Orientation::Upstream => (width, height),
        };
        Ok(Self {
            width,
            height,
            orientation: settings.orientation,
            lines,
            len,
            line: 0,
            time: 0,
            max_len: settings.max_len,
            split: settings.split,
            pixel_notes: settings.pixel_notes,
            inspected: 0,
        })
    }
    pub fn position(&self) -> (u32, u32) {
        (self.line, self.time)
    }
    pub fn dimensions(&self) -> (u32, u32) {
        (self.lines, self.len)
    }
    pub fn inspected(&self) -> u64 {
        self.inspected
    }
    fn at(
        &mut self,
        indices: &[u32],
        check: &mut impl FnMut() -> Result<(), String>,
    ) -> Result<u32, String> {
        if self.inspected.is_multiple_of(CHECK) {
            check()?;
        }
        self.inspected = self
            .inspected
            .checked_add(1)
            .ok_or_else(|| "Palette run work counter exhausted.".to_string())?;
        let index = match self.orientation {
            Orientation::PianoRoll => self.line as usize * self.width as usize + self.time as usize,
            Orientation::Upstream => {
                (self.height - 1 - self.time) as usize * self.width as usize + self.line as usize
            }
        };
        Ok(indices[index])
    }
    fn next(
        &mut self,
        indices: &[u32],
        check: &mut impl FnMut() -> Result<(), String>,
    ) -> Result<Option<Run>, String> {
        while self.line < self.lines {
            if self.time >= self.len {
                self.line += 1;
                self.time = 0;
                continue;
            }
            let color = self.at(indices, check)?;
            if color == u32::MAX {
                self.time += 1;
                continue;
            }
            let start = self.time;
            self.time += 1;
            if !self.pixel_notes {
                while self.time < self.len && self.at(indices, check)? == color {
                    let cut = self.max_len > 0
                        && match self.split {
                            SplitFrom::ImageStart => self.time.is_multiple_of(self.max_len),
                            SplitFrom::NoteStart => self.time - start >= self.max_len,
                        };
                    if cut {
                        break;
                    }
                    self.time += 1;
                }
            }
            return Ok(Some(Run {
                line: self.line,
                start,
                length: self.time - start,
                color,
            }));
        }
        Ok(None)
    }
    /// Visits at most65536runs and checks work every4096cell inspections, including
    /// transparent and long uniform regions. Cursor movement commits only after
    /// every emit/check succeeds. The caller must likewise stage emitted output
    /// privately until success; externally publishing emit callbacks is not atomic.
    pub fn visit_controlled(
        &mut self,
        indices: &[u32],
        max_runs: usize,
        mut emit: impl FnMut(Run) -> Result<(), String>,
        mut check: impl FnMut() -> Result<(), String>,
    ) -> Result<usize, String> {
        if !(1..=MAX_BATCH).contains(&max_runs) {
            return Err("Palette run batch size must be1through65536.".into());
        }
        dimensions(self.width, self.height, indices.len())?;
        check()?;
        let mut next = *self;
        let mut count = 0;
        while count < max_runs {
            let Some(run) = next.next(indices, &mut check)? else {
                break;
            };
            emit(run)?;
            count += 1;
        }
        check()?;
        *self = next;
        Ok(count)
    }
}
#[cfg(test)]
#[path = "image_runs_tests.rs"]
mod tests;
