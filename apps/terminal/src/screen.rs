//! The screen a terminal shows: its main buffer with scrollback, the
//! alternate buffer full-screen programs draw on, the cursor, the OSC 8
//! hyperlinks and the image placements.
//!
//! The emulator keeps one [`Screen`] for the process it runs. It is live
//! output and stays on the device that runs the process. Lines are shared
//! ([`Rc`]): a line the emulator did not touch keeps its allocation, so a
//! redraw converts only the lines that changed.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::graphics::TerminalImagePlacement;

/// One row of a buffer: its text, one character per cell (a wide glyph is
/// followed by a padding space), and the packed attributes of each cell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub attrs: Vec<u8>,
}

/// What a terminal shows. See the [module docs](self).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Screen {
    /// The main buffer, scrollback first: its last rows are the screen.
    pub main: Vec<Rc<Line>>,
    /// The alternate buffer, exactly one screen tall.
    pub alt: Vec<Rc<Line>>,
    /// Whether a full-screen program shows the alternate buffer.
    pub alternate: bool,
    /// The cursor: its column, and its row counted from the top of the
    /// active buffer (scrollback included).
    pub cursor: (u64, u64),
    /// OSC 8 hyperlink targets by the id their cells carry.
    pub links: BTreeMap<u16, String>,
    /// Graphics placed on the grid; their bytes are in the content store.
    pub images: Vec<TerminalImagePlacement>,
}

impl Screen {
    pub fn lines(&self, alternate: bool) -> &Vec<Rc<Line>> {
        if alternate { &self.alt } else { &self.main }
    }

    pub(crate) fn lines_mut(&mut self, alternate: bool) -> &mut Vec<Rc<Line>> {
        if alternate {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    /// The lines of the buffer on screen.
    pub fn active(&self) -> &Vec<Rc<Line>> {
        self.lines(self.alternate)
    }
}
