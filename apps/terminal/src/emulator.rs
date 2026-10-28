//! The terminal emulator: `alacritty_terminal` parses the process's output
//! into its grid, and [`Emulator`] projects that grid onto the [`Screen`] the
//! widget draws, selects in and searches.
//!
//! The projection is incremental. Rows a feed did not touch keep their shared
//! [`Line`], so a redraw converts only what changed; rows that scrolled into
//! history are found without rescanning it (see [`Emulator::arm`]).

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

/// Retained scrollback lines for the main buffer. Under `cfg(test)` it is
/// small so the history limit is reachable without feeding thousands of lines.
#[cfg(not(test))]
pub(crate) const MAX_HISTORY: usize = 5000;
#[cfg(test)]
pub(crate) const MAX_HISTORY: usize = 120;

/// OSC is process-controlled input. Keep its metadata bounded before it can
/// reach the clipboard, a link or the app title.
const MAX_OSC_TITLE_BYTES: usize = 256;
pub(crate) const MAX_OSC_LINK_BYTES: usize = 2 * 1024;
const MAX_OSC_LINKS: usize = 4096;
const MAX_OSC_CLIPBOARD_BYTES: usize = 1024 * 1024;

/// What a feed asks of the widget besides the screen it drew.
#[derive(Default)]
pub(crate) struct Output {
    /// The last OSC 0/2 title set.
    pub title: Option<String>,
    /// How many times the bell rang.
    pub bells: u32,
    /// Replies to capability queries, written back on the process's stdin.
    pub responses: Vec<u8>,
    /// OSC 52 clipboard writes: the text, and whether it targets the primary
    /// selection.
    pub clipboard: Vec<(String, bool)>,
}

/// Collects the emulator's events while a feed parses.
#[derive(Clone, Default)]
struct Events(Rc<RefCell<Vec<Event>>>);

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

/// Synchronized updates (`CSI ? 2026 h`) hold output back until the program
/// ends them or a timer fires. Nothing here would fire that timer while the
/// process is quiet, so the update is shown as it arrives instead.
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

/// Where the main buffer's history stood when a feed began.
#[derive(Clone, Copy)]
struct Armed {
    history: usize,
    alternate: bool,
}

/// The emulator of one process's output. See the [module docs](self).
pub(crate) struct Emulator {
    term: Term<Events>,
    parser: ansi::Processor<Unsynchronized>,
    events: Events,
    /// Kitty graphics travel in APC strings, which the parser drops.
    graphics: GraphicsScanner,
    screen: Screen,
    /// OSC 8 targets by URI, to the ids their cells carry on the screen.
    link_ids: HashMap<String, u16>,
    armed: Option<Armed>,
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
        };
        emulator.project();
        emulator
    }

    pub(crate) fn screen(&self) -> &Screen {
        &self.screen
    }

    /// The cursor's column and row on the screen (scrollback excluded).
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

    /// Reflow both buffers to `columns` × `lines`.
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
                    // The placement's row counts from the top of the
                    // projected buffer, so bring the projection up to here.
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

    /// Note where history stands before parsing, so [`Self::project`] can
    /// tell how many rows scrolled into it. The grid keeps a scrolled-back
    /// view still by advancing its display offset one row per row scrolled
    /// into history; holding the view one row back makes that offset count
    /// them, even once the oldest rows fall off the end of the history.
    fn arm(&mut self) {
        let history = self.term.grid().history_size();
        if history > 0 {
            self.term.scroll_display(Scroll::Delta(1));
        }
        self.armed = Some(Armed {
            history,
            alternate: self.screen.alternate,
        });
    }

    /// Rows scrolled into the main buffer's history since [`Self::arm`], when
    /// that can be told.
    fn scrolled(&self, armed: Armed) -> Option<usize> {
        if armed.alternate || self.screen.alternate {
            return None;
        }
        let grid = self.term.grid();
        let history = grid.history_size();
        if armed.history == 0 {
            // Nothing could be held back; until history is full, its growth
            // is the count.
            return (history < MAX_HISTORY).then_some(history);
        }
        // An offset back at 0 means history was cleared or the terminal
        // reset; one at the limit may have saturated.
        let offset = grid.display_offset();
        (offset > 0 && offset < MAX_HISTORY).then(|| offset - 1)
    }

    /// Bring [`Self::screen`] up to date with the grid.
    fn project(&mut self) {
        let armed = self.armed.take();
        let scrolled = armed.and_then(|armed| self.scrolled(armed).map(|rows| (armed, rows)));
        self.term.scroll_display(Scroll::Bottom);

        let alternate = self.term.mode().contains(TermMode::ALT_SCREEN);
        let grid = self.term.grid();
        let history = grid.history_size();
        let rows = grid.screen_lines();
        let total = history + rows;

        let mut previous = std::mem::take(self.screen.lines_mut(alternate));
        // The rows to convert again start at `keep`; `previous` is aligned so
        // its row `i` is the same buffer row as the projection's `keep + i`.
        let keep = match scrolled {
            Some((armed, scrolled)) if !alternate && previous.len() == armed.history + rows => {
                let dropped = armed.history + scrolled - history;
                previous.drain(..dropped.min(previous.len()));
                self.screen.images.retain_mut(|image| {
                    if image.alternate_screen {
                        return true;
                    }
                    let kept = image.row >= dropped;
                    image.row = image.row.saturating_sub(dropped);
                    kept
                });
                history - scrolled.min(history)
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
        self.screen.alternate = alternate;
        let (column, row) = self.cursor();
        let top = if alternate { 0 } else { history };
        self.screen.cursor = (column as u64, (top + row) as u64);
    }

    /// Store a complete direct Kitty transmission as a content-addressed blob
    /// plus a lightweight placement at the cursor. The renderer validates and
    /// paints this subset, but no protocol success reply is sent: clients must
    /// not infer broad Kitty support from an acknowledgement.
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

/// One grid row as a screen line: one character per cell (a wide glyph is
/// followed by a padding space), with trailing blank cells left off.
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
        // A tab keeps its character in the cell it started at.
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

/// The id cells linking to `uri` carry, registering it on first sight.
/// Unsafe or over-long targets, and links past the registry's bound, are not
/// links.
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
        // The sixteen ANSI colours; the rest name the theme's defaults.
        Color::Named(named) => u8::try_from(named as usize)
            .ok()
            .filter(|index| *index < 16)
            .map_or(CellColor::Default, CellColor::Indexed),
    }
}
