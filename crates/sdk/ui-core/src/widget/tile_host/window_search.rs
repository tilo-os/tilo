//! What a tile's app offers the search of its window, and how a row chosen
//! there reaches the app.

use automerge::AutoCommit;

use super::{TileContent, TileHost};
use crate::widget::WindowPublication;

impl TileHost {
    /// What this tile's app publishes for the search of its window: what
    /// its host reads through its widgets, or what an app keeping its own
    /// document says it publishes
    /// ([`AppWidget::window_search`](crate::widget::AppWidget::window_search)).
    #[must_use]
    pub fn window_search(&self, doc: &AutoCommit) -> WindowPublication {
        if let TileContent::App(app) = &self.content
            && let Some(published) = app.window_search()
        {
            return published;
        }
        WindowPublication {
            resources: crate::widget::published_resources(self, doc),
            providers: crate::widget::published_search_providers(self),
        }
    }

    /// Run `invocation` in an app keeping its own document, through its own
    /// door: it takes no controls through a binding of this tile's.
    pub(super) fn run_in_own_document(
        &mut self,
        invocation: tilo_actions::ControlInvocation,
    ) -> Result<(), tilo_actions::ControlInvocation> {
        match &mut self.content {
            TileContent::App(app) => app.run_window_control(invocation),
            _ => Err(invocation),
        }
    }
}
