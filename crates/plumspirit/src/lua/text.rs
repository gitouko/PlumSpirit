//! Bounded extended grapheme processing. Segmentation is independent of font
//! shaping/rendering, preserves source bytes and never implies glyph support.
use mlua::{BorrowedStr, Lua, LuaString, Table, Value};
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

use super::bridge::{JobLease, NativeBudget, Reservation, Session};
use super::{Category, notes, stop};

#[path = "text_fonts.rs"]
mod fonts;
#[path = "text_layout.rs"]
mod layout;

pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_CLUSTER_BYTES: usize = 64 * 1024;
pub const MAX_GRAPHEMES: usize = 65_536;
pub const MAX_ITERATORS: usize = 64;
const CHUNK_BYTES: usize = 4096;
pub const MAX_SCAN_BYTES: usize = 8 * MAX_TEXT_BYTES;

type Data = Rc<RefCell<Option<State>>>;
#[derive(Default)]
pub(super) struct TextSlot(Vec<Weak<RefCell<Option<State>>>>);
struct State {
    // Validated once without a native text copy. This view pins its Lua string
    // and state, so end_run clears it even from retained iterator closures.
    input: BorrowedStr,
    scanner: Scanner,
    _allocation: Reservation,
}
type Output = (Option<LuaString>, Option<i64>, Option<i64>, Option<i64>);

pub(super) fn end_run(lua: &Lua) {
    fonts::end_run(lua);
    if let Some(mut slot) = lua.app_data_mut::<TextSlot>() {
        for state in slot.0.drain(..).filter_map(|s| s.upgrade()) {
            state.borrow_mut().take();
        }
    }
}

fn source(value: Value) -> mlua::Result<BorrowedStr> {
    let Value::String(text) = value else {
        return Err(stop(Category::Runtime, "Text must be a UTF-8 string."));
    };
    if text.as_bytes().len() > MAX_TEXT_BYTES {
        return Err(stop(
            Category::Budget,
            "Text exceeds the 1 MiB segmentation input limit.",
        ));
    }
    text.to_str().map_err(|_| {
        stop(
            Category::Runtime,
            "Text must be valid UTF-8; input bytes were not replaced.",
        )
    })
}

fn scope(lua: &Lua, job: &Arc<JobLease>) -> mlua::Result<()> {
    job.check().map_err(|e| e.lua())?;
    let current = lua.app_data_ref::<Session>().and_then(|s| s.lease.clone());
    if !current.is_some_and(|s| Arc::ptr_eq(&s, job)) {
        return Err(stop(
            Category::Expired,
            "This grapheme iterator belongs to an earlier job.",
        ));
    }
    notes::check_budget(lua)
}

/// Cursor owns no text or variable-sized scratch. Forward/context chunks are
/// bounded so even a huge combining cluster has cooperative checkpoints.
struct Scanner {
    cursor: GraphemeCursor,
    chunk_start: usize,
    chunk_end: usize,
    boundary: usize,
    count: usize,
    work: usize,
    chunk_bytes: usize,
}
impl Scanner {
    fn new(text: &str, chunk_bytes: usize) -> Self {
        Self {
            cursor: GraphemeCursor::new(0, text.len(), true),
            chunk_start: 0,
            chunk_end: Self::end(text, 0, chunk_bytes),
            boundary: 0,
            count: 0,
            work: 0,
            chunk_bytes,
        }
    }
    fn end(text: &str, start: usize, bytes: usize) -> usize {
        let mut end = (start + bytes).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start && start < text.len() {
            end = start + text[start..].chars().next().unwrap().len_utf8();
        }
        end
    }
    fn charge(&mut self, bytes: usize) -> mlua::Result<()> {
        self.work = self.work.saturating_add(bytes);
        if self.work > MAX_SCAN_BYTES {
            return Err(stop(
                Category::Budget,
                "Unicode segmentation exceeded its bounded scan/context work allowance.",
            ));
        }
        Ok(())
    }
    fn next(
        &mut self,
        text: &str,
        mut check: impl FnMut() -> mlua::Result<()>,
    ) -> mlua::Result<Option<(usize, usize, usize)>> {
        loop {
            check()?;
            let before = self.cursor.cur_cursor();
            let next = self
                .cursor
                .next_boundary(&text[self.chunk_start..self.chunk_end], self.chunk_start);
            self.charge(self.cursor.cur_cursor().saturating_sub(before))?;
            if self.cursor.cur_cursor().saturating_sub(self.boundary) > MAX_CLUSTER_BYTES {
                return Err(stop(
                    Category::Budget,
                    "A grapheme cluster exceeds the 64 KiB limit; it was not split or replaced.",
                ));
            }
            match next {
                Ok(Some(end)) => {
                    if self.count == MAX_GRAPHEMES {
                        return Err(stop(
                            Category::Budget,
                            "Text exceeds the 65,536 grapheme limit.",
                        ));
                    }
                    let start = self.boundary;
                    self.boundary = end;
                    self.count += 1;
                    return Ok(Some((start, end, self.count)));
                }
                Ok(None) => return Ok(None),
                Err(GraphemeIncomplete::NextChunk) => {
                    if self.chunk_end == text.len() {
                        return Err(stop(
                            Category::Internal,
                            "Unicode cursor requested text beyond the input.",
                        ));
                    }
                    let forward = self.chunk_end;
                    // Keep one scalar of left context. At an exact chunk start
                    // the cursor requests fresh RI context despite its known
                    // forward parity; overlap prevents prefix double counting.
                    self.chunk_start = forward - 1;
                    while !text.is_char_boundary(self.chunk_start) {
                        self.chunk_start -= 1;
                    }
                    self.chunk_end = Self::end(text, forward, self.chunk_bytes);
                }
                Err(GraphemeIncomplete::PreContext(end)) => {
                    if end == 0 || end > text.len() || !text.is_char_boundary(end) {
                        return Err(stop(
                            Category::Internal,
                            "Unicode cursor requested invalid context.",
                        ));
                    }
                    let mut start = end.saturating_sub(self.chunk_bytes);
                    while !text.is_char_boundary(start) {
                        start -= 1;
                    }
                    self.charge(end - start)?;
                    check()?;
                    self.cursor.provide_context(&text[start..end], start);
                }
                Err(_) => {
                    return Err(stop(
                        Category::Internal,
                        "Unicode cursor lost its validated text boundary.",
                    ));
                }
            }
        }
    }
}

fn advance(lua: &Lua, data: &Data, job: &Arc<JobLease>) -> mlua::Result<Output> {
    if let Err(error) = scope(lua, job) {
        if let Ok(mut state) = data.try_borrow_mut() {
            state.take();
        }
        return Err(error);
    }
    let mut data = data.try_borrow_mut().map_err(|_| {
        stop(
            Category::Runtime,
            "This grapheme iterator is already in use.",
        )
    })?;
    let Some(state) = data.as_mut() else {
        return Ok((None, None, None, None));
    };
    let result: mlua::Result<Output> = (|| {
        let text = &state.input;
        let Some((start, end, index)) = state.scanner.next(text, || notes::check_budget(lua))?
        else {
            return Ok((None, None, None, None));
        };
        let cluster = lua.create_string(&text.as_bytes()[start..end])?;
        Ok((
            Some(cluster),
            Some((start + 1) as i64),
            Some((end + 1) as i64),
            Some(index as i64),
        ))
    })();
    if result.as_ref().map_or(true, |out| out.0.is_none()) {
        data.take();
    }
    result
}

pub fn install(lua: &Lua, plum: &Table) -> mlua::Result<()> {
    let text = lua.create_table()?;
    text.set(
        "grapheme_count",
        lua.create_function(|lua, value: Value| {
            notes::check_budget(lua)?;
            let input = source(value)?;
            let mut scanner = Scanner::new(&input, CHUNK_BYTES);
            while scanner.next(&input, || notes::check_budget(lua))?.is_some() {}
            Ok(scanner.count)
        })?,
    )?;
    text.set(
        "graphemes",
        lua.create_function(|lua, value: Value| {
            notes::check_budget(lua)?;
            let input = source(value)?;
            let job = lua
                .app_data_ref::<Session>()
                .and_then(|s| s.lease.clone())
                .ok_or_else(|| stop(Category::Expired, "No text job is active."))?;
            scope(lua, &job)?;
            if lua
                .app_data_ref::<TextSlot>()
                .is_none_or(|s| s.0.len() >= MAX_ITERATORS)
            {
                return Err(stop(
                    Category::Budget,
                    "At most 64 grapheme iterators may be created per job.",
                ));
            }
            let budget = lua
                .app_data_ref::<NativeBudget>()
                .ok_or_else(|| stop(Category::Internal, "Native text accounting is unavailable."))?
                .clone();
            let allocation = budget
                .reserve(
                    std::mem::size_of::<State>() + std::mem::size_of::<RefCell<Option<State>>>(),
                )
                .map_err(|e| e.lua())?;
            let scanner = Scanner::new(&input, CHUNK_BYTES);
            let data = Rc::new(RefCell::new(Some(State {
                input,
                scanner,
                _allocation: allocation,
            })));
            lua.app_data_mut::<TextSlot>()
                .unwrap()
                .0
                .push(Rc::downgrade(&data));
            lua.create_function(move |lua, ()| advance(lua, &data, &job))
        })?,
    )?;
    let (major, minor, patch) = unicode_segmentation::UNICODE_VERSION;
    text.set("unicode_version", format!("{major}.{minor}.{patch}"))?;
    for (key, value) in [
        ("max_bytes", MAX_TEXT_BYTES),
        ("max_cluster_bytes", MAX_CLUSTER_BYTES),
        ("max_graphemes", MAX_GRAPHEMES),
        ("max_iterators", MAX_ITERATORS),
        ("max_scan_bytes", MAX_SCAN_BYTES),
    ] {
        text.set(key, value)?;
    }
    fonts::install(lua, &text)?;
    layout::install(lua, &text)?;
    plum.set("text", text)
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
