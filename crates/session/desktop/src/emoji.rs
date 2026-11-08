//! The desk's emoji card: what the session and the chrome say to each other
//! about it.
//!
//! The card itself is the toolkit's own emoji picker ([`ui::widget::EmojiPicker`]),
//! the one every app opens: the chrome frames it, places it and paints it.
//! What crosses the boundary is this link — where the card opens (the caret of
//! the field the person types into, on the display it stands on) and the emoji
//! the person picked, which the session writes into that field.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui::kurbo::Rect;

/// Where the card opens: the caret of the field the person types into and
/// the display it stands on, in the chrome's own coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EmojiPlace {
    /// The display the caret stands on: what a card without a caret is
    /// centred on, and what every card stays within.
    pub viewport: Option<Rect>,
    /// The field's caret; `None` for a field that reported none.
    pub caret: Option<Rect>,
}

/// The card's link with the session: where the next opening goes, and the
/// emoji the person picked until the session has written them into a field.
#[derive(Clone, Default)]
pub struct EmojiLink {
    place: Rc<Cell<Option<EmojiPlace>>>,
    picks: Rc<RefCell<Vec<String>>>,
}

impl EmojiLink {
    /// Where the next opening goes: the caret of the field the person types
    /// into, in the chrome's coordinates.
    pub fn place_at(&self, place: EmojiPlace) {
        self.place.set(Some(place));
    }

    /// Where the card opens now, as the session last said; `None` opens it
    /// centred over the desk.
    #[must_use]
    pub fn place(&self) -> Option<EmojiPlace> {
        self.place.get()
    }

    /// The person picked `text`: it waits for the session to write it into
    /// the field they were typing into.
    pub fn note_picked(&self, text: String) {
        self.picks.borrow_mut().push(text);
    }

    /// The emoji picked since the last call, oldest first.
    pub fn take_picks(&self) -> Vec<String> {
        std::mem::take(&mut *self.picks.borrow_mut())
    }
}
