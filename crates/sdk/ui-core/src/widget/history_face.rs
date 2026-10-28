//! How an app presents its own changes in the shared History view.
//!
//! History is one service: every app's changes are listed, grouped, filtered
//! and restored by the same timeline. What a change *was* is the app's to
//! say. By default a change reads as its sentence ("Changed Formula of
//! y = x"), from the action that made it. An app whose changes are things a
//! person recognises by sight — a calculation, a plotted function — gives
//! the timeline a [`HistoryEntryPresenter`]: for each change it may answer
//! an [`EntryFace`], the row drawn in the app's own typesetting, what Copy
//! puts on the clipboard, and whether choosing the row takes it up in the
//! app instead of previewing the version.
//!
//! A presenter sees only the change's recorded facts ([`HistoryEntry`]: its
//! action, the item and the values it named). It never reaches the live
//! document, so a row reads the same for every member, whoever made it.

use std::rc::Rc;

use vello::kurbo::Rect;

use crate::paint::PaintCtx;
use crate::theme::Theme;
use crate::widget::history::HistoryEntry;

/// An app's presentation of the changes in its History. Asked once per
/// listed change each time the list changes, so answering is cheap: it reads
/// the entry's facts, nothing else.
pub trait HistoryEntryPresenter {
    /// How `entry` shows, or `None` for its sentence.
    fn face(&self, entry: &HistoryEntry) -> Option<EntryFace>;

    /// What a band of `entries` amounts to, said on its second line in
    /// place of how many characters moved ("4 functions, 7 calculations").
    /// `None` keeps the default, which suits text.
    fn group_summary(&self, entries: &[HistoryEntry]) -> Option<String> {
        let _ = entries;
        None
    }
}

/// The content of one History row, painted by the app inside the row's
/// content slot (beside the author, before the time).
pub trait EntryBody {
    /// The row height this content needs under `theme`, in logical pixels.
    /// The timeline lays rows out without a renderer, so this is measured
    /// from the theme's text roles and spacing; the content fits itself
    /// into the rect [`Self::paint`] is given.
    fn height(&self, theme: &Theme) -> f64;
    /// Paint into `rect`, the row's content slot.
    fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect);
}

/// One change as the app presents it.
#[derive(Clone)]
pub struct EntryFace {
    /// What the row says, in words: its accessible name, and what is drawn
    /// where the body is not.
    pub spoken: String,
    /// The row's content in the app's own typesetting.
    pub body: Option<Rc<dyn EntryBody>>,
    /// What Copy puts on the clipboard (a calculation's result).
    pub copy: Option<String>,
    /// Choosing the row takes the change up in the app (a calculation goes
    /// back into the calculator) instead of previewing that version: History
    /// closes and the app is asked to open it
    /// ([`crate::history_mode::capability::PresentsHistoryEntries::open_history_entry`]).
    pub opens: bool,
}

impl EntryFace {
    /// A face that says `spoken` and draws it as the sentence.
    pub fn new(spoken: impl Into<String>) -> Self {
        Self {
            spoken: spoken.into(),
            body: None,
            copy: None,
            opens: false,
        }
    }

    /// Draw `body` instead of the sentence.
    #[must_use]
    pub fn with_body(mut self, body: Rc<dyn EntryBody>) -> Self {
        self.body = Some(body);
        self
    }

    /// Copy puts `text` on the clipboard.
    #[must_use]
    pub fn copying(mut self, text: impl Into<String>) -> Self {
        self.copy = Some(text.into());
        self
    }

    /// Choosing the row opens the change in the app.
    #[must_use]
    pub fn opening(mut self) -> Self {
        self.opens = true;
        self
    }

    /// The row height the face needs under `theme`, at least `minimum`.
    pub(crate) fn height(&self, minimum: f64, theme: &Theme) -> f64 {
        self.body
            .as_ref()
            .map_or(minimum, |body| body.height(theme).max(minimum))
    }
}

impl std::fmt::Debug for EntryFace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryFace")
            .field("spoken", &self.spoken)
            .field("body", &self.body.is_some())
            .field("copy", &self.copy)
            .field("opens", &self.opens)
            .finish()
    }
}
