//! The view's copy of the bytes with its presentation state: cursor, selection, edit
//! mode, column, bytes per row and the changed bytes. A keystroke edits the copy at
//! once and records the [`BufferEdit`] the view submits; the record's state replaces
//! the copy whenever it differs. No rendering, no storage.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
// `#[derive(JsonSchema)]` names the crate as `tilo_resource`.
use ui::tilo_resource::{self, JsonSchema};

use crate::buffer::Hex;

/// One primitive buffer mutation: an insert, delete or overwrite at an offset. Cursor,
/// selection, scroll and the column and endianness settings stay presentation. It is
/// also the edit of the record's bytes at their path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum BufferEdit {
    /// Splice `bytes` in before `offset`, growing the buffer.
    Insert { offset: usize, bytes: Hex },
    /// Remove `len` bytes starting at `offset`.
    Delete { offset: usize, len: usize },
    /// Replace the bytes at `offset..offset+bytes.len()` in place; the buffer never
    /// grows, so bytes past its end are dropped.
    Overwrite { offset: usize, bytes: Hex },
}

impl BufferEdit {
    fn span(&self) -> (usize, usize) {
        match self {
            Self::Insert { offset, bytes } | Self::Overwrite { offset, bytes } => {
                (*offset, bytes.0.len())
            }
            Self::Delete { offset, len } => (*offset, *len),
        }
    }

    /// Whether `next`, right after `self`, continues the same run of typing or deleting.
    #[must_use]
    pub fn continues(&self, next: &Self) -> bool {
        let ((at, len), (next_at, next_len)) = (self.span(), next.span());
        match (self, next) {
            (
                Self::Insert { .. } | Self::Overwrite { .. },
                Self::Insert { .. } | Self::Overwrite { .. },
            ) => next_at >= at && next_at <= at.saturating_add(len),
            // Delete steps in place (Delete) or backwards (Backspace).
            (Self::Delete { .. }, Self::Delete { .. }) => {
                next_at == at || next_at.saturating_add(next_len) == at
            }
            _ => false,
        }
    }
}

/// Which of the two parallel columns the cursor edits. The offset is shared; only the
/// input differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Hex,
    Ascii,
}

impl Column {
    #[must_use]
    pub fn toggle(self) -> Self {
        match self {
            Column::Hex => Column::Ascii,
            Column::Ascii => Column::Hex,
        }
    }
}

/// Overwrite replaces the byte under the cursor; Insert splices a new byte in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    Overwrite,
    Insert,
}

impl EditMode {
    #[must_use]
    pub fn toggle(self) -> Self {
        match self {
            EditMode::Overwrite => EditMode::Insert,
            EditMode::Insert => EditMode::Overwrite,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EditMode::Overwrite => "Overwrite",
            EditMode::Insert => "Insert",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

impl Endian {
    #[must_use]
    pub fn toggle(self) -> Self {
        match self {
            Endian::Little => Endian::Big,
            Endian::Big => Endian::Little,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Endian::Little => "Little endian",
            Endian::Big => "Big endian",
        }
    }
}

pub(crate) fn common_suffix<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    a.iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
}

#[derive(Clone, Debug)]
pub struct HexModel {
    bytes: Vec<u8>,
    /// The cursor's offset (0..=len; the trailing position holds in insert mode only).
    cursor: usize,
    /// The selection spans `min(anchor, cursor)..=max(anchor, cursor)`.
    anchor: Option<usize>,
    /// Bytes that differ from the loaded buffer, which the view paints as changed.
    dirty: BTreeSet<usize>,
    /// The high nibble typed in overwrite mode, which the next digit completes; `None`
    /// between bytes.
    pending_nibble: Option<u8>,
    column: Column,
    mode: EditMode,
    endian: Endian,
    /// Bytes per row: 8 and 16 are the classic choices, any value above 0 works.
    bytes_per_row: usize,
    top_row: usize,
    edits: Vec<BufferEdit>,
}

impl HexModel {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            cursor: 0,
            anchor: None,
            dirty: BTreeSet::new(),
            pending_nibble: None,
            column: Column::Hex,
            mode: EditMode::Overwrite,
            endian: Endian::Little,
            bytes_per_row: 16,
            top_row: 0,
            edits: Vec::new(),
        }
    }

    pub fn take_edits(&mut self) -> Vec<BufferEdit> {
        std::mem::take(&mut self.edits)
    }

    /// Show `bytes`, the buffer as the record holds it now, keeping the cursor
    /// (clamped) and the view settings. Changed bytes are marked against `saved`.
    pub fn show_bytes(&mut self, bytes: &[u8], saved: &[u8]) {
        let unchanged = bytes == self.bytes.as_slice();
        if bytes.len() == saved.len() {
            self.dirty = bytes
                .iter()
                .zip(saved)
                .enumerate()
                .filter_map(|(index, (now, was))| (now != was).then_some(index))
                .collect();
        } else if !unchanged {
            let prefix = self
                .bytes
                .iter()
                .zip(bytes)
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = common_suffix(
                self.bytes.get(prefix..).unwrap_or_default(),
                bytes.get(prefix..).unwrap_or_default(),
            );
            let old_end = self.bytes.len() - suffix;
            let new_end = bytes.len() - suffix;
            self.dirty = self
                .dirty
                .iter()
                .filter_map(|&d| {
                    if d < prefix {
                        Some(d)
                    } else if d >= old_end {
                        Some(d - old_end + new_end)
                    } else {
                        None
                    }
                })
                .chain(prefix..new_end)
                .collect();
        }
        // Local typing already changed the copy; its marks are reconciled against the
        // saved file above.
        if unchanged {
            return;
        }
        // Reuse the buffer: a large file is not reallocated per edit.
        self.bytes.clear();
        self.bytes.extend_from_slice(bytes);
        self.pending_nibble = None;
        self.clamp_cursor();
    }

    pub fn load(&mut self, bytes: Vec<u8>) {
        self.bytes = bytes;
        self.dirty.clear();
        self.edits.clear();
        self.pending_nibble = None;
        self.anchor = None;
        self.cursor = 0;
        self.top_row = 0;
    }

    fn cursor_limit(&self) -> usize {
        match self.mode {
            EditMode::Insert => self.bytes.len(),
            EditMode::Overwrite => self.bytes.len().saturating_sub(1),
        }
    }

    fn clamp_cursor(&mut self) {
        let max = self.cursor_limit();
        self.cursor = self.cursor.min(max);
        self.anchor = self.anchor.map(|anchor| anchor.min(max));
        self.top_row = self.top_row.min(self.row_count().saturating_sub(1));
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn column(&self) -> Column {
        self.column
    }

    pub fn mode(&self) -> EditMode {
        self.mode
    }

    pub fn endian(&self) -> Endian {
        self.endian
    }

    pub fn bytes_per_row(&self) -> usize {
        self.bytes_per_row
    }

    pub fn top_row(&self) -> usize {
        self.top_row
    }

    pub fn pending_nibble(&self) -> Option<u8> {
        self.pending_nibble
    }

    pub fn is_dirty(&self, offset: usize) -> bool {
        self.dirty.contains(&offset)
    }

    pub fn dirty_count(&self) -> usize {
        self.dirty.len()
    }

    /// Forget every changed mark: the buffer matches what is on disk.
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    /// The rows the buffer occupies at the current width; at least 1, so an empty
    /// buffer still shows a cursor row.
    pub fn row_count(&self) -> usize {
        match self.mode {
            EditMode::Insert => self.bytes.len() / self.bytes_per_row + 1,
            EditMode::Overwrite => self.bytes.len().div_ceil(self.bytes_per_row).max(1),
        }
    }

    /// The selection as an inclusive `(start, end)` range.
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.anchor.map(|a| {
            let lo = a.min(self.cursor);
            let hi = a.max(self.cursor);
            (lo, hi.min(self.bytes.len().saturating_sub(1)))
        })
    }

    pub fn selection_len(&self) -> usize {
        match self.selection() {
            Some((lo, hi)) => hi.saturating_sub(lo) + 1,
            None => 0,
        }
    }

    pub fn set_bytes_per_row(&mut self, n: usize) {
        self.bytes_per_row = n.max(1);
    }

    pub fn set_top_row(&mut self, row: usize) {
        let max = self.row_count().saturating_sub(1);
        self.top_row = row.min(max);
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let cur = self.top_row as isize;
        let next = (cur + delta).max(0) as usize;
        self.set_top_row(next);
    }

    /// Scroll the minimum so `row` is inside a `visible_rows`-tall viewport.
    pub fn scroll_into_view(&mut self, row: usize, visible_rows: usize) {
        if visible_rows == 0 {
            return;
        }
        if row < self.top_row {
            self.top_row = row;
        } else if row >= self.top_row + visible_rows {
            self.top_row = row + 1 - visible_rows;
        }
    }

    pub fn toggle_column(&mut self) {
        self.column = self.column.toggle();
        self.pending_nibble = None;
    }

    pub fn set_column(&mut self, col: Column) {
        if self.column != col {
            self.pending_nibble = None;
        }
        self.column = col;
    }

    pub fn toggle_mode(&mut self) {
        self.mode = self.mode.toggle();
        self.pending_nibble = None;
        self.clamp_cursor();
    }

    pub fn toggle_endian(&mut self) {
        self.endian = self.endian.toggle();
    }

    /// Move the cursor to `offset`, clamped to the buffer. `extend` keeps the
    /// selection anchor; otherwise the selection clears.
    pub fn move_to(&mut self, offset: usize, extend: bool) {
        let max = if extend {
            self.bytes.len().saturating_sub(1)
        } else {
            self.cursor_limit()
        };
        let target = offset.min(max);
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = if self.bytes.is_empty() { 0 } else { target };
        self.pending_nibble = None;
    }

    pub fn move_left(&mut self, extend: bool) {
        let target = self.cursor.saturating_sub(1);
        self.move_to(target, extend);
    }

    pub fn move_right(&mut self, extend: bool) {
        self.move_to(self.cursor + 1, extend);
    }

    pub fn move_up(&mut self, extend: bool) {
        if self.cursor >= self.bytes_per_row {
            self.move_to(self.cursor - self.bytes_per_row, extend);
        } else {
            self.move_to(0, extend);
        }
    }

    pub fn move_down(&mut self, extend: bool) {
        self.move_to(self.cursor + self.bytes_per_row, extend);
    }

    pub fn move_row_start(&mut self, extend: bool) {
        let start = (self.cursor / self.bytes_per_row) * self.bytes_per_row;
        self.move_to(start, extend);
    }

    pub fn move_row_end(&mut self, extend: bool) {
        let start = (self.cursor / self.bytes_per_row) * self.bytes_per_row;
        self.move_to(start + self.bytes_per_row - 1, extend);
    }

    pub fn move_doc_start(&mut self, extend: bool) {
        self.move_to(0, extend);
    }

    pub fn move_doc_end(&mut self, extend: bool) {
        self.move_to(self.cursor_limit(), extend);
    }

    pub fn page_up(&mut self, rows: usize, extend: bool) {
        let delta = rows * self.bytes_per_row;
        self.move_to(self.cursor.saturating_sub(delta), extend);
    }

    pub fn page_down(&mut self, rows: usize, extend: bool) {
        let delta = rows * self.bytes_per_row;
        self.move_to(self.cursor + delta, extend);
    }

    pub fn select_all(&mut self) {
        if self.bytes.is_empty() {
            return;
        }
        self.anchor = Some(0);
        self.cursor = self.bytes.len() - 1;
        self.pending_nibble = None;
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    pub fn cursor_cell(&self) -> (usize, usize) {
        (
            self.cursor / self.bytes_per_row,
            self.cursor % self.bytes_per_row,
        )
    }

    /// Set the byte at `offset`; `false` when it is out of bounds.
    pub fn set_byte(&mut self, offset: usize, value: u8) -> bool {
        if let Some(slot) = self.bytes.get_mut(offset) {
            if *slot != value {
                *slot = value;
                self.dirty.insert(offset);
                self.edits.push(BufferEdit::Overwrite {
                    offset,
                    bytes: Hex(vec![value]),
                });
            }
            true
        } else {
            false
        }
    }

    /// Insert `value` before `offset`, shifting the changed marks at and after it.
    pub fn insert_byte(&mut self, offset: usize, value: u8) {
        let at = offset.min(self.bytes.len());
        self.bytes.insert(at, value);
        let shifted: BTreeSet<usize> = self
            .dirty
            .iter()
            .map(|&d| if d >= at { d + 1 } else { d })
            .collect();
        self.dirty = shifted;
        self.dirty.insert(at);
        self.edits.push(BufferEdit::Insert {
            offset: at,
            bytes: Hex(vec![value]),
        });
    }

    /// Remove the byte at `offset` and shift the later marks down; `None` when out of
    /// bounds.
    pub fn delete_byte(&mut self, offset: usize) -> Option<u8> {
        if offset >= self.bytes.len() {
            return None;
        }
        let removed = self.bytes.remove(offset);
        let shifted: BTreeSet<usize> = self
            .dirty
            .iter()
            .filter(|&&d| d != offset)
            .map(|&d| if d > offset { d - 1 } else { d })
            .collect();
        self.dirty = shifted;
        self.edits.push(BufferEdit::Delete { offset, len: 1 });
        Some(removed)
    }

    /// Remove `len` bytes at `offset` as one [`BufferEdit::Delete`]. Marks inside the
    /// range are dropped; those after it shift down.
    fn remove_range(&mut self, offset: usize, len: usize) {
        let end = (offset + len).min(self.bytes.len());
        if offset >= end {
            return;
        }
        let removed = end - offset;
        self.bytes.drain(offset..end);
        let shifted: BTreeSet<usize> = self
            .dirty
            .iter()
            .filter_map(|&d| {
                if d < offset {
                    Some(d)
                } else if d >= end {
                    Some(d - removed)
                } else {
                    None
                }
            })
            .collect();
        self.dirty = shifted;
        self.edits.push(BufferEdit::Delete {
            offset,
            len: removed,
        });
    }

    /// Type one hex digit: in overwrite mode the first sets the high nibble and the
    /// second completes the byte, in insert mode each completed byte is spliced in.
    /// `true` when the digit was consumed.
    pub fn input_hex_digit(&mut self, digit: u8) -> bool {
        if digit > 0xF {
            return false;
        }
        match self.mode {
            EditMode::Overwrite => {
                // Empty buffer: nothing to overwrite, so insert instead.
                if self.cursor >= self.bytes.len() {
                    return self.input_hex_digit_insert(digit);
                }
                match self.pending_nibble.take() {
                    None => {
                        let cur = self.bytes.get(self.cursor).copied().unwrap_or_default();
                        let new = (digit << 4) | (cur & 0x0F);
                        self.set_byte(self.cursor, new);
                        self.pending_nibble = Some(digit);
                    }
                    Some(_high) => {
                        let cur = self.bytes.get(self.cursor).copied().unwrap_or_default();
                        let new = (cur & 0xF0) | digit;
                        self.set_byte(self.cursor, new);
                        self.move_right(false);
                    }
                }
                true
            }
            EditMode::Insert => self.input_hex_digit_insert(digit),
        }
    }

    fn input_hex_digit_insert(&mut self, digit: u8) -> bool {
        match self.pending_nibble.take() {
            None => {
                // The byte exists at once with a zero low nibble, so it shows while
                // typing.
                self.insert_byte(self.cursor, digit << 4);
                self.pending_nibble = Some(digit);
                true
            }
            Some(_high) => {
                if let Some(cur) = self.bytes.get(self.cursor).copied() {
                    let new = (cur & 0xF0) | digit;
                    let _ = self.set_byte(self.cursor, new);
                }
                self.cursor += 1;
                true
            }
        }
    }

    /// Type a raw character into the ASCII column, then advance.
    pub fn input_ascii_char(&mut self, ch: char) -> bool {
        if !ch.is_ascii() {
            return false;
        }
        let b = ch as u8;
        match self.mode {
            EditMode::Overwrite => {
                if self.cursor < self.bytes.len() {
                    self.set_byte(self.cursor, b);
                    self.move_right(false);
                } else {
                    self.insert_byte(self.cursor, b);
                    self.cursor += 1;
                }
            }
            EditMode::Insert => {
                self.insert_byte(self.cursor, b);
                self.cursor += 1;
            }
        }
        self.pending_nibble = None;
        true
    }

    /// In insert mode delete the byte before the cursor and step back; in overwrite
    /// mode only step back.
    pub fn backspace(&mut self) {
        match self.mode {
            EditMode::Insert => {
                if self.cursor > 0 {
                    self.delete_byte(self.cursor - 1);
                    self.cursor -= 1;
                }
            }
            EditMode::Overwrite => {
                self.move_left(false);
            }
        }
        self.pending_nibble = None;
    }

    /// Delete the selection, or the byte at the cursor, leaving the cursor there.
    pub fn delete_at_cursor(&mut self) {
        if let Some((lo, hi)) = self.selection() {
            self.remove_range(lo, hi - lo + 1);
            self.anchor = None;
            self.cursor = lo.min(self.bytes.len().saturating_sub(1));
        } else if self.cursor < self.bytes.len() {
            self.delete_byte(self.cursor);
            if self.cursor >= self.bytes.len() {
                self.cursor = self.bytes.len().saturating_sub(1);
            }
        }
        self.pending_nibble = None;
    }

    /// Replace each of `spans` (ascending byte ranges, as find returns them) with `with`.
    /// A span overlapping the one before it is left as it is, and in overwrite mode a
    /// replacement of another length is refused. The cursor lands on the first
    /// replacement; `Err` when overwrite mode cannot take it.
    pub fn replace_spans(
        &mut self,
        spans: &[(usize, usize)],
        with: &[u8],
    ) -> Result<usize, ChangesLength> {
        let mut kept: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
        for &(start, end) in spans {
            let after_last = kept.last().is_none_or(|&(_, last_end)| start >= last_end);
            if start < end && end <= self.bytes.len() && after_last {
                kept.push((start, end));
            }
        }
        if self.mode == EditMode::Overwrite && kept.iter().any(|&(s, e)| e - s != with.len()) {
            return Err(ChangesLength);
        }
        let mut changed = 0;
        // Last to first: each edit's offset is where its span still is.
        for &(start, end) in kept.iter().rev() {
            if end - start == with.len() {
                if self.overwrite_range(start, with) {
                    changed += 1;
                }
            } else {
                self.remove_range(start, end - start);
                self.insert_range(start, with);
                changed += 1;
            }
        }
        if let Some(&(first, _)) = kept.first() {
            self.anchor = None;
            self.cursor = first;
            self.pending_nibble = None;
            self.clamp_cursor();
        }
        Ok(changed)
    }

    fn overwrite_range(&mut self, offset: usize, bytes: &[u8]) -> bool {
        let Some(slots) = self.bytes.get_mut(offset..offset + bytes.len()) else {
            return false;
        };
        if slots == bytes {
            return false;
        }
        for (index, (slot, &value)) in slots.iter_mut().zip(bytes).enumerate() {
            if *slot != value {
                *slot = value;
                self.dirty.insert(offset + index);
            }
        }
        self.edits.push(BufferEdit::Overwrite {
            offset,
            bytes: Hex(bytes.to_vec()),
        });
        true
    }

    /// Insert `bytes` before `offset` as one edit, marking them changed.
    fn insert_range(&mut self, offset: usize, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let at = offset.min(self.bytes.len());
        let n = bytes.len();
        self.bytes.splice(at..at, bytes.iter().copied());
        self.dirty = self
            .dirty
            .iter()
            .map(|&d| if d >= at { d + n } else { d })
            .chain(at..at + n)
            .collect();
        self.edits.push(BufferEdit::Insert {
            offset: at,
            bytes: Hex(bytes.to_vec()),
        });
    }
}

/// A replacement in overwrite mode that would change the buffer's length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangesLength;

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> HexModel {
        HexModel::new((0u8..=63).collect())
    }

    #[test]
    fn insert_navigation_reaches_append_cell_and_records_an_append() {
        let mut m = HexModel::new(vec![0xaa; 16]);
        m.toggle_mode();
        m.move_doc_end(false);
        assert_eq!(m.cursor(), 16);
        assert_eq!(m.row_count(), 2);
        m.move_left(false);
        m.move_right(false);
        assert_eq!(m.cursor(), 16);
        m.show_bytes(&[0xbb; 16], &[0xaa; 16]);
        assert_eq!(m.cursor(), 16, "presentation keeps the append cursor");
        m.input_hex_digit(0xc);
        m.input_hex_digit(0xd);
        assert_eq!(m.bytes().last(), Some(&0xcd));
        assert_eq!(m.len(), 17);
        assert_eq!(
            m.take_edits(),
            vec![
                BufferEdit::Insert {
                    offset: 16,
                    bytes: Hex(vec![0xc0])
                },
                BufferEdit::Overwrite {
                    offset: 16,
                    bytes: Hex(vec![0xcd])
                },
            ]
        );
        m.toggle_mode();
        assert_eq!(m.cursor(), 16, "overwrite stays on an existing byte");
    }

    #[test]
    fn cursor_movement_clamps_and_selection_is_inclusive() {
        let mut m = model();
        m.move_left(false);
        assert_eq!(m.cursor(), 0);
        m.move_doc_end(false);
        assert_eq!(m.cursor(), 63);
        m.move_right(false);
        assert_eq!(m.cursor(), 63);

        let mut m = model();
        m.set_bytes_per_row(16);
        m.move_down(false);
        assert_eq!(m.cursor(), 16);
        m.move_up(false);
        assert_eq!(m.cursor(), 0);

        let mut m = model();
        m.move_to(10, false);
        m.move_to(5, true);
        assert_eq!(m.selection(), Some((5, 10)));
        assert_eq!(m.selection_len(), 6);

        let mut m = model();
        m.select_all();
        assert_eq!(m.selection(), Some((0, 63)));
        assert_eq!(m.selection_len(), 64);
    }

    #[test]
    fn byte_edits_overwrite_insert_delete_and_track_dirty_and_history() {
        let mut m = HexModel::new(vec![0x00, 0x00]);
        // Type "ff" over the first byte.
        assert!(m.input_hex_digit(0xF));
        assert_eq!(m.bytes()[0], 0xF0); // high nibble first
        assert!(m.input_hex_digit(0xF));
        assert_eq!(m.bytes()[0], 0xFF);
        assert_eq!(m.cursor(), 1); // advanced after completing the byte
        assert!(m.is_dirty(0));

        let mut edited = HexModel::new(vec![0x10, 0x20]);
        assert!(edited.set_byte(1, 0xAB));
        assert!(
            edited.set_byte(1, 0xAB),
            "an unchanged byte records nothing"
        );
        edited.move_to(0, false);
        edited.move_to(1, true);
        edited.delete_at_cursor();
        assert_eq!(
            edited.take_edits(),
            [
                BufferEdit::Overwrite {
                    offset: 1,
                    bytes: Hex(vec![0xAB])
                },
                BufferEdit::Delete { offset: 0, len: 2 },
            ]
        );
        assert!(edited.take_edits().is_empty());

        let mut m = HexModel::new(vec![0xAA]);
        m.toggle_mode(); // -> Insert
        m.move_to(0, false);
        m.input_hex_digit(0x1);
        m.input_hex_digit(0x2);
        assert_eq!(m.bytes(), &[0x12, 0xAA]);

        let mut m = HexModel::new(vec![0x00, 0x00]);
        m.set_column(Column::Ascii);
        assert!(m.input_ascii_char('A'));
        assert_eq!(m.bytes()[0], b'A');
        assert_eq!(m.cursor(), 1);

        let mut m = model();
        m.move_to(2, false);
        m.move_to(5, true);
        m.delete_at_cursor();
        assert_eq!(m.len(), 60);
        // The byte originally at offset 6 is now at offset 2.
        assert_eq!(m.bytes()[2], 6);

        let mut m = HexModel::new(vec![0x10, 0x20, 0x30]);
        m.set_byte(2, 0x31); // dirty at 2
        assert!(m.is_dirty(2));
        m.insert_byte(0, 0xFF); // everything shifts up
        assert!(m.is_dirty(0)); // the inserted byte
        assert!(m.is_dirty(3)); // 0x31 moved from 2 -> 3
        assert!(!m.is_dirty(2));
    }

    #[test]
    fn shown_bytes_keep_the_cursor_and_mark_what_differs_from_the_saved_file() {
        let saved = vec![0x10, 0x20, 0x30, 0x40];
        let mut m = HexModel::new(saved.clone());
        m.move_to(3, false);
        // An edit from elsewhere marks its byte; undoing it clears the mark.
        m.show_bytes(&[0x10, 0xFF, 0x30, 0x40], &saved);
        assert!(m.is_dirty(1));
        assert_eq!(m.cursor(), 3);
        m.show_bytes(&saved, &saved);
        assert_eq!(m.dirty_count(), 0);
        m.show_bytes(&[0x10, 0x40], &saved);
        assert_eq!(m.cursor(), 1);
        assert_eq!(m.dirty_count(), 0, "a pure removal changes no byte left");
        m.show_bytes(&[0x10, 0x99, 0x40], &saved);
        assert!(m.is_dirty(1));
        assert!(!m.is_dirty(2));
        m.load(vec![1, 2, 3]);
        assert_eq!(m.dirty_count(), 0);
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn viewport_scrolls_minimally_and_cycles_row_width() {
        // scroll_into_view scrolls the minimum.
        let mut m = HexModel::new((0u8..=255).collect());
        m.set_bytes_per_row(16); // 16 rows total
        m.scroll_into_view(10, 4);
        assert_eq!(m.top_row(), 7); // 10 + 1 - 4
        m.scroll_into_view(2, 4);
        assert_eq!(m.top_row(), 2);
    }

    #[test]
    fn overwrite_mode_replaces_spans_of_the_same_length_and_refuses_another_length() {
        let mut m = HexModel::new(vec![0xde, 0xad, 0x00, 0xde, 0xad]);
        assert_eq!(m.mode(), EditMode::Overwrite);
        assert_eq!(
            m.replace_spans(&[(0, 2), (3, 5)], &[0xbe]),
            Err(ChangesLength),
            "overwrite never changes the length"
        );
        assert_eq!(m.bytes(), &[0xde, 0xad, 0x00, 0xde, 0xad]);
        assert!(
            m.take_edits().is_empty(),
            "a refused replacement edits nothing"
        );

        assert_eq!(m.replace_spans(&[(0, 2), (3, 5)], &[0xbe, 0xef]), Ok(2));
        assert_eq!(m.bytes(), &[0xbe, 0xef, 0x00, 0xbe, 0xef]);
        assert_eq!(m.dirty_count(), 4);
        assert_eq!(m.cursor(), 0, "the cursor lands on the first replacement");
        assert_eq!(
            m.take_edits(),
            vec![
                BufferEdit::Overwrite {
                    offset: 3,
                    bytes: Hex(vec![0xbe, 0xef])
                },
                BufferEdit::Overwrite {
                    offset: 0,
                    bytes: Hex(vec![0xbe, 0xef])
                },
            ]
        );
    }

    #[test]
    fn insert_mode_replaces_spans_with_bytes_of_another_length() {
        let mut m = HexModel::new(b"aaXaa".to_vec());
        m.toggle_mode();
        // Overlapping spans (as find returns for "aa" in "aaa") keep the first.
        assert_eq!(m.replace_spans(&[(0, 2), (1, 3), (3, 5)], b"bbb"), Ok(2));
        assert_eq!(m.bytes(), b"bbbXbbb");
        assert_eq!(m.replace_spans(&[(3, 4)], b""), Ok(1));
        assert_eq!(m.bytes(), b"bbbbbb");
        let mut replayed = b"aaXaa".to_vec();
        for edit in m.take_edits() {
            match edit {
                BufferEdit::Insert { offset, bytes } => {
                    replayed.splice(offset..offset, bytes.0);
                }
                BufferEdit::Delete { offset, len } => {
                    replayed.drain(offset..offset + len);
                }
                BufferEdit::Overwrite { offset, bytes } => {
                    replayed[offset..offset + bytes.0.len()].copy_from_slice(&bytes.0);
                }
            }
        }
        assert_eq!(
            replayed, b"bbbbbb",
            "the recorded edits make the same bytes"
        );
    }
}
