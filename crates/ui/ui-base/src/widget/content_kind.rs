//! What a sidebar's content is, so the layout knows the narrowest it works at.

/// What the shell's content is, so it knows the narrowest the content still works at; below that
/// the window shows one column at a time. Apps say what their content is, never how wide it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContentKind {
    /// A list or table of items: files, messages, tracks, events
    /// (`metrics.list_min_width`).
    List,
    /// A reading column: a message, an article, a forecast
    /// (`metrics.reading_min_width`).
    Reading,
    /// A page of settings, a form or a page of facts
    /// (`metrics.page_min_width`). The default.
    #[default]
    Page,
    /// A grid of media tiles: albums, photos, videos
    /// (`metrics.grid_min_width`).
    Grid,
    /// A document or editor the window is for, whose sidebar (an outline, the list of documents) is
    /// visited now and then: docking needs a reading column, so narrower it floats as a drawer
    /// rather than taking turns.
    Document,
    /// A surface drawn on, zoomed and panned: as narrow as a grid of tiles works, so its inspector
    /// docks sooner and the surface stays in view.
    Canvas,
}

impl ContentKind {
    /// The narrowest this kind of content still works at, from `metrics`
    /// (`theme.tokens.metrics`, or [`crate::tokens::Metrics::BASE`] at 1×).
    #[must_use]
    pub fn min_width(self, metrics: &crate::tokens::Metrics) -> f32 {
        match self {
            Self::List => metrics.list_min_width,
            Self::Reading | Self::Document => metrics.reading_min_width,
            Self::Page => metrics.page_min_width,
            Self::Grid | Self::Canvas => metrics.grid_min_width,
        }
    }

    /// The width class content of this kind is in at the least beside a sidebar: a list, a page, a
    /// reading column or a document is a column of text, so it is no phone's screen there; a grid
    /// of tiles or a surface drawn on works narrower than a column, which is what its own minimum
    /// states.
    #[must_use]
    pub fn beside_sidebar_class(self) -> crate::widget::WidthClass {
        match self {
            Self::List | Self::Reading | Self::Page | Self::Document => {
                crate::widget::WidthClass::Medium
            }
            Self::Grid | Self::Canvas => crate::widget::WidthClass::Compact,
        }
    }

    /// Whether the content is the window's point and its sidebar an
    /// occasional visit: too narrow to dock, the sidebar floats over it.
    #[must_use]
    pub fn is_content_primary(self) -> bool {
        matches!(self, Self::Document | Self::Canvas)
    }
}
