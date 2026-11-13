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
use crate::inner::{Attr, CellColor, TermModes, safe_osc_text};
use crate::screen::{Line, Screen};

#[cfg(not(test))]
pub(crate) const MAX_HISTORY: usize = 5000;
#[cfg(test)]
pub(crate) const MAX_HISTORY: usize = 120;

const MAX_OSC_TITLE_BYTES: usize = 256;
pub(crate) const MAX_OSC_LINK_BYTES: usize = 2 * 1024;
const MAX_OSC_LINKS: usize = 4096;
const MAX_OSC_CLIPBOARD_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub(crate) struct Output {
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

pub(crate) struct Emulator {
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
    pub(crate) fn new(columns: usize, lines: usize) -> Self {
        let events = Events::default();
        let config = Config {
            scrolling_history: MAX_HISTORY,
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

    pub(crate) fn screen(&self) -> &Screen {
        &self.screen
    }

    pub(crate) fn cursor(&self) -> (usize, usize) {
        let point = self.term.grid().cursor.point;
        (point.column.0, usize::try_from(point.line.0).unwrap_or(0))
    }

    pub(crate) fn modes(&self) -> TermModes {
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

    pub(crate) fn resize(&mut self, columns: usize, lines: usize) {
        self.term.resize(Size {
            columns: columns.max(1),
            lines: lines.max(1),
        });
        self.armed = None;
        self.project();
    }

    /// Parse `bytes` onto the screen. `allow_clipboard` admits OSC 52 writes.
    pub(crate) fn feed(
        &mut self,
        bytes: &[u8],
        content: &ContentStore,
        allow_clipboard: bool,
    ) -> Output {
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
            if offset == 0 || offset >= MAX_HISTORY {
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
