//! A compact VT/ANSI terminal emulator.
//!
//! The SSH session hands us the raw byte stream from the remote PTY; this
//! turns it into a fixed-size grid of styled [`Cell`]s that the session
//! surface renders. It is deliberately small — enough of xterm's grammar
//! to make an interactive login shell (prompts, `ls --color`, `htop`-style
//! cursor moves, clears) read correctly — not a full terminal.
//!
//! Parsing is delegated to `vte` (the same state machine the standalone
//! terminal app uses); we only implement the [`vte::Perform`] callbacks and
//! the grid model.

use crate::session::{Cell, TermLine};
use vte::{Params, Parser, Perform};

/// A live terminal grid driven by a byte stream. Owns the `vte` parser and
/// the cell buffer; call [`VtGrid::feed`] with PTY bytes, then read
/// [`VtGrid::lines`] to render.
pub struct VtGrid {
    parser: Parser,
    grid: Grid,
}

impl VtGrid {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            parser: Parser::new(),
            grid: Grid::new(cols.max(1), rows.max(1)),
        }
    }

    /// Feed a chunk of remote PTY output into the emulator.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.grid, bytes);
    }

    /// The current cursor cell (col, row), for drawing the block caret.
    pub fn cursor(&self) -> (usize, usize) {
        (self.grid.cx, self.grid.cy)
    }

    /// Snapshot the grid as render-ready lines. Trailing all-blank rows are
    /// kept so the caret has somewhere to sit; each row is trimmed of its
    /// trailing blanks to keep the scene light.
    pub fn lines(&self) -> Vec<TermLine> {
        self.grid
            .cells
            .iter()
            .map(|row| {
                let last = row.iter().rposition(|c| c.ch != ' ' || c.color.is_some());
                let end = last.map_or(0, |i| i + 1);
                TermLine {
                    // `end` is one past a position found within `row` (or 0), so it
                    // never exceeds `row.len()`; the fallback is unreachable.
                    cells: row.get(..end).unwrap_or_default().to_vec(),
                }
            })
            .collect()
    }
}

/// The mutable cell buffer + cursor + pen. Separate from [`VtGrid`] so it
/// can be the `&mut self` the `vte::Perform` callbacks mutate while the
/// parser borrows `VtGrid` immutably.
struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Vec<Cell>>,
    cx: usize,
    cy: usize,
    pen_color: Option<usize>,
    pen_bold: bool,
}

impl Grid {
    fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            cells: vec![vec![Cell::blank(); cols]; rows],
            cx: 0,
            cy: 0,
            pen_color: None,
            pen_bold: false,
        }
    }

    /// Move to the start of the next line, scrolling the buffer up if the
    /// cursor is already on the last row.
    fn newline(&mut self) {
        if self.cy + 1 >= self.rows {
            self.cells.remove(0);
            self.cells.push(vec![Cell::blank(); self.cols]);
        } else {
            self.cy += 1;
        }
    }

    fn put(&mut self, ch: char) {
        if self.cx >= self.cols {
            // Auto-wrap to the next line.
            self.cx = 0;
            self.newline();
        }
        if let Some(row) = self.cells.get_mut(self.cy)
            && let Some(cell) = row.get_mut(self.cx)
        {
            *cell = Cell {
                ch,
                color: self.pen_color,
                bold: self.pen_bold,
            };
        }
        self.cx += 1;
    }

    fn erase_line_from_cursor(&mut self) {
        if let Some(row) = self.cells.get_mut(self.cy) {
            for cell in row.iter_mut().skip(self.cx) {
                *cell = Cell::blank();
            }
        }
    }

    fn erase_whole_line(&mut self) {
        if let Some(row) = self.cells.get_mut(self.cy) {
            for cell in row.iter_mut() {
                *cell = Cell::blank();
            }
        }
    }

    fn clear_all(&mut self) {
        for row in &mut self.cells {
            for cell in row.iter_mut() {
                *cell = Cell::blank();
            }
        }
    }

    /// Apply a Select-Graphic-Rendition parameter list to the pen.
    fn apply_sgr(&mut self, params: &Params) {
        let mut any = false;
        for p in params {
            any = true;
            let code = p.first().copied().unwrap_or(0);
            match code {
                0 => {
                    self.pen_color = None;
                    self.pen_bold = false;
                }
                1 => self.pen_bold = true,
                22 => self.pen_bold = false,
                39 => self.pen_color = None,
                30..=37 => self.pen_color = Some((code - 30) as usize),
                90..=97 => self.pen_color = Some((code - 90 + 8) as usize),
                _ => {}
            }
        }
        if !any {
            // A bare `ESC [ m` is SGR reset.
            self.pen_color = None;
            self.pen_bold = false;
        }
    }
}

/// First numeric parameter, defaulting to `default` when absent or zero-ish.
fn first_param(params: &Params, default: u16) -> u16 {
    match params.iter().next().and_then(|p| p.first()).copied() {
        Some(0) | None => default,
        Some(n) => n,
    }
}

impl Perform for Grid {
    fn print(&mut self, c: char) {
        self.put(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => self.newline(),
            b'\r' => self.cx = 0,
            0x08 => self.cx = self.cx.saturating_sub(1),
            b'\t' => {
                // Advance to the next 8-column tab stop.
                let next = ((self.cx / 8) + 1) * 8;
                self.cx = next.min(self.cols.saturating_sub(1));
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, _intermediates: &[u8], _ignore: bool, c: char) {
        match c {
            'm' => self.apply_sgr(params),
            'H' | 'f' => {
                // Cursor position: 1-based (row, col).
                let mut it = params.iter();
                let row = it
                    .next()
                    .and_then(|p| p.first())
                    .copied()
                    .unwrap_or(1)
                    .max(1);
                let col = it
                    .next()
                    .and_then(|p| p.first())
                    .copied()
                    .unwrap_or(1)
                    .max(1);
                self.cy = (row as usize - 1).min(self.rows.saturating_sub(1));
                self.cx = (col as usize - 1).min(self.cols.saturating_sub(1));
            }
            'A' => self.cy = self.cy.saturating_sub(first_param(params, 1) as usize),
            'B' => {
                self.cy =
                    (self.cy + first_param(params, 1) as usize).min(self.rows.saturating_sub(1));
            }
            'C' => {
                self.cx =
                    (self.cx + first_param(params, 1) as usize).min(self.cols.saturating_sub(1));
            }
            'D' => self.cx = self.cx.saturating_sub(first_param(params, 1) as usize),
            'G' => self.cx = (first_param(params, 1) as usize - 1).min(self.cols.saturating_sub(1)),
            'J' => {
                // Erase in display: 2 = whole screen (and home the cursor is
                // common but not required); 0 = to end (approximate as line).
                match params
                    .iter()
                    .next()
                    .and_then(|p| p.first())
                    .copied()
                    .unwrap_or(0)
                {
                    2 | 3 => self.clear_all(),
                    _ => self.erase_line_from_cursor(),
                }
            }
            'K' => {
                match params
                    .iter()
                    .next()
                    .and_then(|p| p.first())
                    .copied()
                    .unwrap_or(0)
                {
                    2 => self.erase_whole_line(),
                    _ => self.erase_line_from_cursor(),
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_text(line: &TermLine) -> String {
        line.cells.iter().map(|c| c.ch).collect()
    }

    #[test]
    fn text_prints_wraps_overwrites_and_scrolls() {
        eprintln!("phase prints_text_and_wraps_lines");
        {
            let mut g = VtGrid::new(20, 5);
            g.feed(b"hello\r\nworld");
            let lines = g.lines();
            assert_eq!(plain_text(&lines[0]), "hello");
            assert_eq!(plain_text(&lines[1]), "world");
        }

        eprintln!("phase carriage_return_overwrites");
        {
            let mut g = VtGrid::new(20, 3);
            g.feed(b"aaaa\rbb");
            assert_eq!(plain_text(&g.lines()[0]), "bbaa");
        }

        eprintln!("phase newline_scrolls_when_cursor_at_bottom");
        {
            let mut g = VtGrid::new(8, 2);
            g.feed(b"a\r\nb\r\nc");
            // Grid is 2 rows: "a" scrolled off, leaving "b" then "c".
            let lines = g.lines();
            assert_eq!(plain_text(&lines[0]), "b");
            assert_eq!(plain_text(&lines[1]), "c");
        }
    }

    #[test]
    fn sgr_sets_colors_bold_and_bright_palette() {
        eprintln!("phase sgr_sets_color_and_bold");
        {
            let mut g = VtGrid::new(20, 3);
            g.feed(b"\x1b[1;31mX\x1b[0my");
            let row = &g.lines()[0].cells;
            assert_eq!(row[0].ch, 'X');
            assert_eq!(row[0].color, Some(1));
            assert!(row[0].bold);
            assert_eq!(row[1].ch, 'y');
            assert_eq!(row[1].color, None);
            assert!(!row[1].bold);
        }

        eprintln!("phase bright_color_maps_into_upper_palette");
        {
            let mut g = VtGrid::new(10, 2);
            g.feed(b"\x1b[92mg"); // bright green -> index 2 + 8 = 10
            assert_eq!(g.lines()[0].cells[0].color, Some(10));
        }
    }

    #[test]
    fn cursor_erase_clear_and_resize_edit_the_grid() {
        eprintln!("phase cursor_position_and_erase");
        {
            let mut g = VtGrid::new(10, 4);
            g.feed(b"line0\r\nline1");
            // Home the cursor, erase to end of the first line, write new text.
            g.feed(b"\x1b[1;1H\x1b[Knew");
            assert_eq!(plain_text(&g.lines()[0]), "new");
            assert_eq!(plain_text(&g.lines()[1]), "line1");
        }

        eprintln!("phase clear_screen_blanks_grid");
        {
            let mut g = VtGrid::new(8, 3);
            g.feed(b"junk\r\nmore");
            g.feed(b"\x1b[2J");
            for line in g.lines() {
                assert_eq!(plain_text(&line).trim(), "");
            }
        }
    }
}
