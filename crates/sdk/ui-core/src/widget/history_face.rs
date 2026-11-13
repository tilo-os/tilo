//! How an app presents its own changes in the shared History view.
//!
//! A presenter sees only the change's recorded facts ([`HistoryEntry`]: its action, the
//! item and the values it named). It never reaches the live document, so a row reads the
//! same for every member, whoever made it.

use std::rc::Rc;

use vello::kurbo::Rect;

use crate::paint::PaintCtx;
use crate::theme::Theme;
use crate::widget::history::HistoryEntry;

/// An app's presentation of the changes in its History. Asked once per
/// listed change each time the list changes, so answering is cheap: it reads
/// the entry's facts, nothing else.
pub trait HistoryEntryPresenter {
    fn face(&self, entry: &HistoryEntry) -> Option<EntryFace>;

    fn group_summary(&self, entries: &[HistoryEntry]) -> Option<String> {
        let _ = entries;
        None
    }
}

/// The content of one History row, painted by the app inside the row's
/// content slot (beside the author, before the time).
pub trait EntryBody {
    fn height(&self, theme: &Theme) -> f64;
    fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect);
}

/// One change as the app presents it.
#[derive(Clone)]
pub struct EntryFace {
    pub spoken: String,
    pub body: Option<Rc<dyn EntryBody>>,
    pub copy: Option<String>,
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
