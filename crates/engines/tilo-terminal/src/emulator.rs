//! The terminal emulator: `alacritty_terminal` parses the process's output
//! into its grid, and [`Emulator`] projects that grid onto the [`Screen`] the
//! widget draws, selects in and searches.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{ClipboardType, Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Color, Timeout};
use tilo_content::ContentStore;

use crate::graphics::{GraphicsScanner, StreamEvent, TerminalImagePlacement, parse_kitty};
use crate::screen::{Line, Screen};
use crate::{Attr, CellColor, TermModes, safe_osc_text};

/// Rows of scrollback a terminal keeps behind its grid.
pub const DEFAULT_HISTORY: usize = 5000;
/// Rows a test keeps: enough to scroll, few enough that its checks stay fast.
pub const TEST_HISTORY: usize = 120;

const MAX_OSC_TITLE_BYTES: usize = 256;
/// The longest OSC 8 link a cell may carry.
pub const MAX_OSC_LINK_BYTES: usize = 2 * 1024;
const MAX_OSC_LINKS: usize = 4096;
const MAX_OSC_CLIPBOARD_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub struct Output {
    pub title: Option<String>,
    pub bells: u32,
    pub responses: Vec<u8>,
    pub clipboard: Vec<(String, bool)>,
}

#[derive(Clone, Default)]
struct Events(Rc<RefCell<Vec<Event>>>);

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

#[derive(Default)]
struct Unsynchronized;

impl Timeout for Unsynchronized {
    fn set_timeout(&mut self, _: std::time::Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

struct Size {
    columns: usize,
    lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

#[derive(Clone, Copy)]
struct Armed {
    held: bool,
    alternate: bool,
}

pub struct Emulator {
    term: Term<Events>,
    parser: ansi::Processor<Unsynchronized>,
    events: Events,
    graphics: GraphicsScanner,
    screen: Screen,
    link_ids: HashMap<String, u16>,
    armed: Option<Armed>,
    rows: Vec<*const Cell>,
    history: usize,
}

impl Emulator {
    #[must_use]
    pub fn new(columns: usize, lines: usize) -> Self {
        Self::with_history(columns, lines, DEFAULT_HISTORY)
    }

    /// An emulator that keeps `history` rows of scrollback.
    #[must_use]
    pub fn with_history(columns: usize, lines: usize, history: usize) -> Self {
        let events = Events::default();
        let config = Config {
            scrolling_history: history,
            ..Config::default()
        };
        let term = Term::new(
            config,
            &Size {
                columns: columns.max(1),
                lines: lines.max(1),
            },
            events.clone(),
        );
        let mut emulator = Self {
            term,
            parser: ansi::Processor::new(),
            events,
            graphics: GraphicsScanner::default(),
            screen: Screen::default(),
            link_ids: HashMap::new(),
            armed: None,
            rows: Vec::new(),
            history: 0,
        };
        emulator.project();
        emulator
    }

    #[must_use]
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    #[must_use]
    pub fn cursor(&self) -> (usize, usize) {
        let point = self.term.grid().cursor.point;
        (point.column.0, usize::try_from(point.line.0).unwrap_or(0))
    }

    #[must_use]
    pub fn modes(&self) -> TermModes {
        let mode = *self.term.mode();
        let mouse_tracking = if mode.contains(TermMode::MOUSE_MOTION) {
            1003
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            1002
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            1000
        } else {
            0
        };
        TermModes {
            cursor_visible: mode.contains(TermMode::SHOW_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            app_cursor_keys: mode.contains(TermMode::APP_CURSOR),
            mouse_report: mouse_tracking != 0,
            mouse_tracking,
            mouse_sgr: mode.contains(TermMode::SGR_MOUSE),
            focus_report: mode.contains(TermMode::FOCUS_IN_OUT),
        }
    }

    pub fn resize(&mut self, columns: usize, lines: usize) {
        self.term.resize(Size {
            columns: columns.max(1),
            lines: lines.max(1),
        });
        self.armed = None;
        self.project();
    }

    /// Parse `bytes` onto the screen. `allow_clipboard` admits OSC 52 writes.
    pub fn feed(&mut self, bytes: &[u8], content: &ContentStore, allow_clipboard: bool) -> Output {
        self.arm();
        for event in self.graphics.push(bytes) {
            match event {
                StreamEvent::Bytes(bytes) => self.parser.advance(&mut self.term, &bytes),
                StreamEvent::Kitty(payload) => {
                    self.project();
                    self.arm();
                    self.kitty_graphics(&payload, content);
                }
            }
        }
        self.project();

        let mut output = Output::default();
        for event in self.events.0.borrow_mut().drain(..) {
            match event {
                Event::Title(title) => {
                    if let Some(title) = safe_osc_text(title.as_bytes(), MAX_OSC_TITLE_BYTES) {
                        output.title = Some(title);
                    }
                }
                Event::Bell => output.bells = output.bells.saturating_add(1),
                Event::PtyWrite(reply) => output.responses.extend_from_slice(reply.as_bytes()),
                Event::ClipboardStore(target, text)
                    if allow_clipboard && text.len() <= MAX_OSC_CLIPBOARD_BYTES =>
                {
                    output
                        .clipboard
                        .push((text, target == ClipboardType::Selection));
                }
                _ => {}
            }
        }
        output
    }

    fn arm(&mut self) {
        let held = self.term.grid().history_size() > 0;
        if held {
            self.term.scroll_display(Scroll::Delta(1));
        }
        self.armed = Some(Armed {
            held,
            alternate: self.screen.alternate,
        });
    }

    /// How many rows at the top of the history the projection already has, unchanged from `dropped` on.
    /// History rows never change in place, so only the screen's rows are converted again: a feed may have
    /// written them before they scrolled.
    fn kept(&self, armed: Armed) -> Option<(usize, usize)> {
        if armed.alternate || self.screen.alternate {
            return None;
        }
        let grid = self.term.grid();
        if armed.held {
            let offset = grid.display_offset();
            if offset == 0 || offset >= DEFAULT_HISTORY {
                return None;
            }
        }
        let history = grid.history_size();
        if history == 0 || self.history == 0 {
            return Some((0, 0));
        }
        let top = grid.topmost_line();
        let first = grid[top][..].as_ptr();
        let dropped = self.rows.iter().position(|row| std::ptr::eq(*row, first))?;
        let kept = self.history.checked_sub(dropped)?.min(history);
        let last = kept.checked_sub(1)?;
        let same = self
            .rows
            .get(dropped + last)
            .is_some_and(|row| std::ptr::eq(*row, grid[top + last][..].as_ptr()));
        same.then_some((dropped, kept))
    }

    fn project(&mut self) {
        let kept = self.armed.take().and_then(|armed| self.kept(armed));
        self.term.scroll_display(Scroll::Bottom);

        let alternate = self.term.mode().contains(TermMode::ALT_SCREEN);
        let grid = self.term.grid();
        let history = grid.history_size();
        let total = history + grid.screen_lines();

        let mut previous = std::mem::take(self.screen.lines_mut(alternate));
        let keep = match kept {
            Some((dropped, kept)) if !alternate => {
                previous.drain(..dropped.min(previous.len()));
                self.screen.images.retain_mut(|image| {
                    if image.alternate_screen {
                        return true;
                    }
                    let kept = image.row >= dropped;
                    image.row = image.row.saturating_sub(dropped);
                    kept
                });
                kept
            }
            _ => 0,
        };
        let tail = previous.split_off(keep.min(previous.len()));
        let mut lines = previous;
        let top = grid.topmost_line();
        for index in keep..total {
            let row = &grid[top + index];
            let line = convert(&row[..], &mut self.link_ids, &mut self.screen.links);
            match tail.get(index - keep) {
                Some(shown) if **shown == line => lines.push(Rc::clone(shown)),
                _ => lines.push(Rc::new(line)),
            }
        }
        *self.screen.lines_mut(alternate) = lines;
        if !alternate {
            self.rows = (0..total)
                .map(|index| grid[top + index][..].as_ptr())
                .collect();
            self.history = history;
        }
        self.screen.alternate = alternate;
        let (column, row) = self.cursor();
        let top = if alternate { 0 } else { history };
        self.screen.cursor = (column as u64, (top + row) as u64);
    }

    /// Store a complete direct Kitty transmission as a content-addressed blob and a placement at the
    /// cursor. No protocol success reply is sent, so clients do not infer more support than this subset.
    fn kitty_graphics(&mut self, payload: &[u8], content: &ContentStore) {
        let Some(image) = parse_kitty(payload) else {
            return;
        };
        if image.mime == "image/png" && tilo_content::sniff(&image.bytes).mime != "image/png" {
            return;
        }
        if image.columns == 0 || image.rows == 0 || image.columns > 1024 || image.rows > 1024 {
            return;
        }
        let hash = match content.put(&image.bytes) {
            Ok(hash) => hash,
            Err(error) => {
                tracing::warn!(%error, "terminal: couldn't store graphics payload");
                return;
            }
        };
        let (col, row) = self.screen.cursor;
        self.screen.images.push(TerminalImagePlacement {
            hash,
            mime: image.mime.to_string(),
            byte_len: image.bytes.len() as u64,
            row: usize::try_from(row).unwrap_or(0),
            col: usize::try_from(col).unwrap_or(0),
            columns: image.columns,
            rows: image.rows,
            pixel_width: image.pixel_width,
            pixel_height: image.pixel_height,
            protocol_id: image.protocol_id,
            alternate_screen: self.screen.alternate,
        });
    }
}

fn convert(
    cells: &[Cell],
    link_ids: &mut HashMap<String, u16>,
    links: &mut std::collections::BTreeMap<u16, String>,
) -> Line {
    let mut text = String::with_capacity(cells.len());
    let mut attrs = Vec::with_capacity(cells.len());
    for cell in cells {
        let flags = cell.flags;
        let link_id = cell
            .hyperlink()
            .map_or(0, |link| link_id(link.uri(), link_ids, links));
        attrs.push(Attr {
            fg: cell_color(cell.fg),
            bg: cell_color(cell.bg),
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline: flags.intersects(Flags::ALL_UNDERLINES),
            reverse: flags.contains(Flags::INVERSE),
            dim: flags.contains(Flags::DIM),
            strike: flags.contains(Flags::STRIKEOUT),
            is_wide_cont: flags.contains(Flags::WIDE_CHAR_SPACER),
            link_id,
        });
        let spacer = flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
        text.push(if spacer || cell.c == '\t' {
            ' '
        } else {
            cell.c
        });
    }
    while text.ends_with(' ') && attrs.last() == Some(&Attr::default()) {
        text.pop();
        attrs.pop();
    }
    Line { text, attrs }
}

fn link_id(
    uri: &str,
    link_ids: &mut HashMap<String, u16>,
    links: &mut std::collections::BTreeMap<u16, String>,
) -> u16 {
    if let Some(&id) = link_ids.get(uri) {
        return id;
    }
    if links.len() >= MAX_OSC_LINKS {
        return 0;
    }
    let Some(url) = safe_osc_text(uri.as_bytes(), MAX_OSC_LINK_BYTES) else {
        return 0;
    };
    let Some(id) = u16::try_from(links.len() + 1).ok() else {
        return 0;
    };
    links.insert(id, url.clone());
    link_ids.insert(url, id);
    id
}

fn cell_color(color: Color) -> CellColor {
    match color {
        Color::Spec(rgb) => CellColor::Rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(index) => CellColor::Indexed(index),
        Color::Named(named) => u8::try_from(named as usize)
            .ok()
            .filter(|index| *index < 16)
            .map_or(CellColor::Default, CellColor::Indexed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CellColor;

    /// The bytes a shell prints are the text the screen shows once its escapes are applied: the
    /// colours and weights stay as cells' attributes, a title is reported, and a carriage return
    /// rewrites the row it lands on.
    #[test]
    fn styled_output_reads_as_the_text_the_screen_shows() {
        let content = ContentStore::memory();
        let mut emulator = Emulator::with_history(20, 5, TEST_HISTORY);
        emulator.feed(
            b"\x1b[1;31merror\x1b[0m: it broke\r\nok\r\n",
            &content,
            false,
        );
        let lines: Vec<&str> = emulator
            .screen()
            .active()
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(&lines[..2], ["error: it broke", "ok"]);
        assert!(lines[2..].iter().all(|line| line.is_empty()));
        let error = &emulator.screen().active()[0].attrs;
        assert!(error[0].bold, "SGR 1 is the cell's weight");
        assert_ne!(
            error[0].fg,
            CellColor::Default,
            "SGR 31 is the cell's colour"
        );
        assert!(!error[5].bold, "SGR 0 ends the style");
        assert_eq!(error[5].fg, CellColor::Default);

        let rewritten = emulator.feed(b"10%\r50%\r100%\r\nbroke\r\n", &content, false);
        assert_eq!(rewritten.title, None);
        let lines: Vec<&str> = emulator
            .screen()
            .active()
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(
            &lines[2..4],
            ["100%", "broke"],
            "a carriage return rewrites the row it returns to"
        );
    }

    /// A program's title and the link a cell belongs to are reported as they are set, and the
    /// link is one the screen's own id names.
    #[test]
    fn a_program_sets_the_title_and_a_cells_link() {
        let content = ContentStore::memory();
        let mut emulator = Emulator::with_history(20, 5, TEST_HISTORY);
        let printed = emulator.feed(
            b"\x1b]0;build\x07\x1b]8;;https://example.test/doc\x1b\\clicked\x1b]8;;\x1b\\",
            &content,
            false,
        );
        assert_eq!(printed.title.as_deref(), Some("build"));
        let line = &emulator.screen().active()[0];
        assert_eq!(line.text, "clicked");
        let id = line.attrs[0].link_id;
        assert_ne!(id, 0, "the text belongs to the link it was printed in");
        assert_eq!(
            emulator.screen().links.get(&id).map(String::as_str),
            Some("https://example.test/doc")
        );
        assert!(
            line.attrs[..7].iter().all(|attr| attr.link_id == id),
            "every cell the words cover belongs to the link"
        );
    }

    /// The kitty keyboard protocol is pushed and popped around a program without the cursor
    /// moving: the pushes are the program's keyboard, not its drawing.
    #[test]
    fn kitty_keyboard_pushes_and_pops_do_not_move_the_cursor() {
        let content = ContentStore::memory();
        let mut emulator = Emulator::with_history(20, 5, TEST_HISTORY);
        emulator.feed(b"abc", &content, false);
        assert_eq!(emulator.cursor(), (3, 0));
        emulator.feed(b"\x1b[>1u", &content, false);
        assert_eq!(emulator.cursor(), (3, 0), "a push moves nothing");
        emulator.feed(b"\x1b[<u", &content, false);
        assert_eq!(emulator.cursor(), (3, 0), "a pop moves nothing either");
        assert_eq!(emulator.screen().active()[0].text, "abc");
    }

    /// Only the rows new output changed are new lines: the output above them is the same lines,
    /// which a frame that draws the grid again pays nothing for.
    #[test]
    fn only_the_rows_that_changed_are_new() {
        let content = ContentStore::memory();
        let mut emulator = Emulator::with_history(20, 5, TEST_HISTORY);
        emulator.feed(b"one\r\ntwo\r\n", &content, false);
        let before: Vec<*const crate::screen::Line> =
            emulator.screen().main.iter().map(Rc::as_ptr).collect();
        emulator.feed(b"three\r\n", &content, false);
        let after: Vec<*const crate::screen::Line> =
            emulator.screen().main.iter().map(Rc::as_ptr).collect();
        assert_eq!(
            before[0], after[0],
            "a line above the new output stays the line it was"
        );
        assert_eq!(before[1], after[1]);
        assert_ne!(before[2], after[2], "the line the output landed on is new");
        // Rewriting one row in place leaves the lines above it as they were.
        emulator.feed(b"\rdone", &content, false);
        let rewritten: Vec<*const crate::screen::Line> =
            emulator.screen().main.iter().map(Rc::as_ptr).collect();
        for row in 0..3 {
            assert_eq!(after[row], rewritten[row], "row {row} did not change");
        }
        assert_ne!(after[3], rewritten[3], "the rewritten row is a new line");
        assert_eq!(emulator.screen().active()[3].text, "done");
    }

    /// A query a program asks is answered on its own input, and a status report is one.
    #[test]
    fn a_programs_query_is_answered_on_its_input() {
        let content = ContentStore::memory();
        let mut emulator = Emulator::with_history(20, 5, TEST_HISTORY);
        let printed = emulator.feed(b"\x1b[6n", &content, false);
        assert_eq!(
            String::from_utf8_lossy(&printed.responses),
            "\x1b[1;1R",
            "the device answers where the cursor is"
        );
        let printed = emulator.feed(b"\x1b[c", &content, false);
        assert!(!printed.responses.is_empty(), "the device says what it is");
    }
}
