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
            resources: crate::bound_dispatch::published_resources(self, doc),
            providers: crate::bound_dispatch::published_search_providers(self),
        }
    }
}
