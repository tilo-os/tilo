//! The screen a terminal shows: its main buffer with scrollback, the
//! alternate buffer full-screen programs draw on, the cursor, the OSC 8
//! hyperlinks and the image placements.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::graphics::TerminalImagePlacement;
use crate::inner::Attr;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub attrs: Vec<Attr>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Screen {
    pub main: Vec<Rc<Line>>,
    pub alt: Vec<Rc<Line>>,
    pub alternate: bool,
    pub cursor: (u64, u64),
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

    pub fn active(&self) -> &Vec<Rc<Line>> {
        self.lines(self.alternate)
    }
}
