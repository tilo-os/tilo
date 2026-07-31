//! Pure data model for the hex editor.
//!
//! Holds the editable byte buffer plus the editor's view state — cursor
//! position, selection, edit mode, active column, bytes-per-row, and the
//! set of bytes that have been modified since load. None of this knows
//! about rendering or the CRDT; it is plain logic so every operation is
//! unit-testable in isolation.

use std::collections::BTreeSet;

/// One primitive buffer mutation the model performed, recorded so the view can
/// replay it through the qOS action model instead of persisting the whole blob.
///
/// These are the *only* document-mutating operations a hex editor has — every
/// keystroke that changes the bytes reduces to an insert, a delete, or an
/// overwrite at a concrete offset. Cursor movement, selection, scroll and the
/// mode/column/endian toggles never appear here: they are presentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BufferEdit {
    /// Splice `bytes` in *before* `offset`, growing the buffer.
    Insert { offset: usize, bytes: Vec<u8> },
    /// Remove `len` bytes starting at `offset`.
    Delete { offset: usize, len: usize },
    /// Replace the bytes already at `offset..offset+bytes.len()` in place.
    Overwrite { offset: usize, bytes: Vec<u8> },
}

/// Which of the two parallel columns the keyboard cursor is editing.
/// The cursor offset is shared; only the input semantics differ — hex
/// digits in the hex column, raw characters in the ASCII column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Hex,
    Ascii,
}

impl Column {
    pub fn toggle(self) -> Self {
        match self {
            Column::Hex => Column::Ascii,
            Column::Ascii => Column::Hex,
        }
    }
}

/// Overwrite replaces the byte under the cursor; Insert splices a new
/// byte in, growing the buffer. Classic hex-editor distinction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    Overwrite,
    Insert,
}

impl EditMode {
    pub fn toggle(self) -> Self {
        match self {
            EditMode::Overwrite => EditMode::Insert,
            EditMode::Insert => EditMode::Overwrite,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EditMode::Overwrite => "OVR",
            EditMode::Insert => "INS",
        }
    }
}

/// Byte-endianness for the data inspector and multi-byte search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

impl Endian {
    pub fn toggle(self) -> Self {
        match self {
            Endian::Little => Endian::Big,
            Endian::Big => Endian::Little,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Endian::Little => "LE",
            Endian::Big => "BE",
        }
    }
}

/// The full editor state: the bytes plus where we are in them.
#[derive(Clone, Debug)]
pub struct HexModel {
    /// The byte buffer being edited.
    bytes: Vec<u8>,
    /// Cursor offset into `bytes` (0..=len; the trailing position is
    /// valid only in insert mode / for an append cursor).
    cursor: usize,
    /// Selection anchor. When present, the selection spans
    /// `min(anchor, cursor)..=max(anchor, cursor)` inclusive of both.
    anchor: Option<usize>,
    /// Bytes that differ from the originally-loaded buffer. Drives the
    /// "changed byte" highlight so a reviewer can see edits at a glance.
    dirty: BTreeSet<usize>,
    /// While typing the high nibble of a hex byte in overwrite mode we
    /// stash it here; the next hex digit completes the byte. `None`
    /// means the cursor is between bytes (clean nibble boundary).
    pending_nibble: Option<u8>,
    column: Column,
    mode: EditMode,
    endian: Endian,
    /// How many byte columns to render per row. 8 and 16 are the
    /// classic choices; the model accepts any value >= 1.
    bytes_per_row: usize,
    /// First row visible in the viewport (scroll position, in rows).
    top_row: usize,
    /// Buffer mutations performed since the last [`take_edits`](Self::take_edits).
    /// The view drains these each event and dispatches each as a replayable
    /// [`crate::actions::HexEditorAction`]; the model itself never touches the
    /// document. Cleared whenever the buffer is adopted wholesale.
    pending_edits: Vec<BufferEdit>,
}

impl HexModel {
    /// Build a model around an initial byte buffer.
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
            pending_edits: Vec::new(),
        }
    }

    // --- buffer-edit journal --------------------------------------------

    /// Drain the buffer mutations performed since the last call. The view
    /// dispatches each through the qOS action model so it replays on peers and
    /// records one undo boundary. Presentation (cursor, selection, scroll,
    /// mode) is never journaled here.
    pub fn take_edits(&mut self) -> Vec<BufferEdit> {
        std::mem::take(&mut self.pending_edits)
    }

    /// Replace the buffer with an externally-authoritative version — a peer's
    /// edit, a Ctrl+K palette action, or an undo restoring the document. The
    /// bytes are the shared truth, so the render cache re-derives from them:
    /// the cursor is clamped back in range and transient nibble / selection /
    /// dirty state that no longer maps is dropped. This produces no journal
    /// entry (the change originated elsewhere).
    pub fn adopt_bytes(&mut self, bytes: Vec<u8>) {
        self.bytes = bytes;
        let max = self.bytes.len().saturating_sub(1);
        if self.cursor > max {
            self.cursor = max;
        }
        self.anchor = None;
        self.pending_nibble = None;
        self.dirty.clear();
        self.pending_edits.clear();
    }

    // --- read accessors -------------------------------------------------

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

    /// Forget every pending edit marker — the buffer now matches what is on
    /// disk. Called after a successful save so the "changed byte" highlight
    /// and the edited-count clear.
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    /// Total number of rows the buffer occupies at the current width.
    /// Always at least 1 so an empty buffer still shows one cursor row.
    pub fn row_count(&self) -> usize {
        if self.bytes.is_empty() {
            1
        } else {
            self.bytes.len().div_ceil(self.bytes_per_row)
        }
    }

    /// Selection as an inclusive `(start, end)` byte range, normalised so
    /// `start <= end`. `None` when there is no active selection.
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.anchor.map(|a| {
            let lo = a.min(self.cursor);
            let hi = a.max(self.cursor);
            (lo, hi.min(self.bytes.len().saturating_sub(1)))
        })
    }

    /// Length of the active selection in bytes (0 when none).
    pub fn selection_len(&self) -> usize {
        match self.selection() {
            Some((lo, hi)) => hi.saturating_sub(lo) + 1,
            None => 0,
        }
    }

    /// The byte at the cursor, if the cursor is within bounds.
    pub fn byte_at_cursor(&self) -> Option<u8> {
        self.bytes.get(self.cursor).copied()
    }

    // --- view configuration --------------------------------------------

    pub fn set_bytes_per_row(&mut self, n: usize) {
        self.bytes_per_row = n.max(1);
    }

    /// Cycle 16 -> 8 -> 16. The two classic layouts.
    pub fn cycle_bytes_per_row(&mut self) {
        self.bytes_per_row = match self.bytes_per_row {
            16 => 8,
            _ => 16,
        };
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

    /// Ensure `row` is within the `visible_rows`-tall viewport, scrolling
    /// the minimum amount needed. Called after cursor moves.
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

    // --- mode toggles ---------------------------------------------------

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
    }

    pub fn toggle_endian(&mut self) {
        self.endian = self.endian.toggle();
    }

    // --- cursor movement ------------------------------------------------

    /// Move the cursor to an absolute offset, clamped to the buffer.
    /// `extend` keeps/creates the selection anchor; otherwise the
    /// selection is cleared.
    pub fn move_to(&mut self, offset: usize, extend: bool) {
        let max = self.bytes.len().saturating_sub(1);
        let target = offset.min(max.max(0));
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
        self.move_to(self.bytes.len().saturating_sub(1), extend);
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

    /// The (row, col) grid position of the cursor at the current width.
    pub fn cursor_cell(&self) -> (usize, usize) {
        (
            self.cursor / self.bytes_per_row,
            self.cursor % self.bytes_per_row,
        )
    }

    // --- editing --------------------------------------------------------

    /// Set the byte at `offset` to `value`, recording it as dirty.
    /// No-op (returns false) if `offset` is out of bounds.
    pub fn set_byte(&mut self, offset: usize, value: u8) -> bool {
        if let Some(slot) = self.bytes.get_mut(offset) {
            if *slot != value {
                *slot = value;
                self.dirty.insert(offset);
                self.pending_edits
                    .push(BufferEdit::Overwrite { offset, bytes: vec![value] });
            }
            true
        } else {
            false
        }
    }

    /// Insert `value` before `offset`, growing the buffer and marking the
    /// new byte dirty. Offsets at/after the insertion point shift up, so
    /// their dirty flags shift too.
    pub fn insert_byte(&mut self, offset: usize, value: u8) {
        let at = offset.min(self.bytes.len());
        self.bytes.insert(at, value);
        // Shift dirty markers at/after the insertion point.
        let shifted: BTreeSet<usize> = self
            .dirty
            .iter()
            .map(|&d| if d >= at { d + 1 } else { d })
            .collect();
        self.dirty = shifted;
        self.dirty.insert(at);
        self.pending_edits
            .push(BufferEdit::Insert { offset: at, bytes: vec![value] });
    }

    /// Remove the byte at `offset`. Returns the removed byte, or None if
    /// out of bounds. Dirty markers after the deletion shift down.
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
        self.pending_edits
            .push(BufferEdit::Delete { offset, len: 1 });
        Some(removed)
    }

    /// Remove `len` bytes starting at `offset` as one contiguous edit (the
    /// selection-delete path), recording a single [`BufferEdit::Delete`] so the
    /// keystroke and the palette's "Delete selection" build the identical
    /// action. Dirty markers inside the range are dropped; those after it shift
    /// down.
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
        self.pending_edits
            .push(BufferEdit::Delete { offset, len: removed });
    }

    /// Type a single hex digit (`0-9a-f`) at the cursor. In overwrite
    /// mode the first digit sets the high nibble (pending) and the second
    /// completes the byte and advances. In insert mode each completed
    /// byte is spliced in. Returns true if the digit was consumed.
    pub fn input_hex_digit(&mut self, digit: u8) -> bool {
        if digit > 0xF {
            return false;
        }
        match self.mode {
            EditMode::Overwrite => {
                // Empty buffer: nothing to overwrite — treat as insert so
                // the user can start typing into a blank document.
                if self.cursor >= self.bytes.len() {
                    return self.input_hex_digit_insert(digit);
                }
                match self.pending_nibble.take() {
                    None => {
                        let cur = self.bytes[self.cursor];
                        let new = (digit << 4) | (cur & 0x0F);
                        self.set_byte(self.cursor, new);
                        self.pending_nibble = Some(digit);
                    }
                    Some(_high) => {
                        let cur = self.bytes[self.cursor];
                        let new = (cur & 0xF0) | digit;
                        self.set_byte(self.cursor, new);
                        // Byte complete — advance.
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
                // Start a fresh byte with the high nibble; the byte exists
                // immediately (low nibble 0) so it's visible while typing.
                self.insert_byte(self.cursor, digit << 4);
                self.pending_nibble = Some(digit);
                true
            }
            Some(_high) => {
                let cur = self.bytes[self.cursor];
                let new = (cur & 0xF0) | digit;
                if new != cur {
                    self.bytes[self.cursor] = new;
                    self.pending_edits.push(BufferEdit::Overwrite {
                        offset: self.cursor,
                        bytes: vec![new],
                    });
                }
                self.cursor += 1;
                true
            }
        }
    }

    /// Type a raw character into the ASCII column. Overwrites or inserts
    /// the byte per the active mode, then advances.
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

    /// Backspace: in insert mode deletes the byte before the cursor and
    /// steps back; in overwrite mode just steps back (non-destructive).
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

    /// Delete the active selection (or the single byte at the cursor when
    /// there is no selection). Leaves the cursor at the deletion point.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> HexModel {
        HexModel::new((0u8..=63).collect())
    }

    #[test]
    fn cursor_moves_clamp_to_bounds() {
        let mut m = model();
        m.move_left(false);
        assert_eq!(m.cursor(), 0);
        m.move_doc_end(false);
        assert_eq!(m.cursor(), 63);
        m.move_right(false);
        assert_eq!(m.cursor(), 63);
    }

    #[test]
    fn vertical_movement_uses_row_width() {
        let mut m = model();
        m.set_bytes_per_row(16);
        m.move_down(false);
        assert_eq!(m.cursor(), 16);
        m.move_up(false);
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn selection_is_inclusive_and_normalised() {
        let mut m = model();
        m.move_to(10, false);
        m.move_to(5, true);
        assert_eq!(m.selection(), Some((5, 10)));
        assert_eq!(m.selection_len(), 6);
    }

    #[test]
    fn select_all_spans_buffer() {
        let mut m = model();
        m.select_all();
        assert_eq!(m.selection(), Some((0, 63)));
        assert_eq!(m.selection_len(), 64);
    }

    #[test]
    fn overwrite_hex_two_nibbles_sets_byte_and_advances() {
        let mut m = HexModel::new(vec![0x00, 0x00]);
        // Type "ff" over the first byte.
        assert!(m.input_hex_digit(0xF));
        assert_eq!(m.bytes()[0], 0xF0); // high nibble first
        assert!(m.input_hex_digit(0xF));
        assert_eq!(m.bytes()[0], 0xFF);
        assert_eq!(m.cursor(), 1); // advanced after completing the byte
        assert!(m.is_dirty(0));
    }

    #[test]
    fn insert_mode_hex_grows_buffer() {
        let mut m = HexModel::new(vec![0xAA]);
        m.toggle_mode(); // -> Insert
        m.move_to(0, false);
        m.input_hex_digit(0x1);
        m.input_hex_digit(0x2);
        assert_eq!(m.bytes(), &[0x12, 0xAA]);
    }

    #[test]
    fn ascii_input_overwrites() {
        let mut m = HexModel::new(vec![0x00, 0x00]);
        m.set_column(Column::Ascii);
        assert!(m.input_ascii_char('A'));
        assert_eq!(m.bytes()[0], b'A');
        assert_eq!(m.cursor(), 1);
    }

    #[test]
    fn delete_selection_removes_range() {
        let mut m = model();
        m.move_to(2, false);
        m.move_to(5, true);
        m.delete_at_cursor();
        assert_eq!(m.len(), 60);
        // The byte originally at offset 6 is now at offset 2.
        assert_eq!(m.bytes()[2], 6);
    }

    #[test]
    fn insert_shifts_dirty_markers() {
        let mut m = HexModel::new(vec![0x10, 0x20, 0x30]);
        m.set_byte(2, 0x31); // dirty at 2
        assert!(m.is_dirty(2));
        m.insert_byte(0, 0xFF); // everything shifts up
        assert!(m.is_dirty(0)); // the inserted byte
        assert!(m.is_dirty(3)); // 0x31 moved from 2 -> 3
        assert!(!m.is_dirty(2));
    }

    #[test]
    fn scroll_into_view_scrolls_minimum() {
        let mut m = HexModel::new((0u8..=255).collect());
        m.set_bytes_per_row(16); // 16 rows total
        m.scroll_into_view(10, 4);
        assert_eq!(m.top_row(), 7); // 10 + 1 - 4
        m.scroll_into_view(2, 4);
        assert_eq!(m.top_row(), 2);
    }

    #[test]
    fn cycle_bytes_per_row_flips_16_8() {
        let mut m = model();
        assert_eq!(m.bytes_per_row(), 16);
        m.cycle_bytes_per_row();
        assert_eq!(m.bytes_per_row(), 8);
        m.cycle_bytes_per_row();
        assert_eq!(m.bytes_per_row(), 16);
    }
}
