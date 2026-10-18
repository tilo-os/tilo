//! A message the window shows above its content: private browsing, a failed
//! private-window launch, a tab that cannot close yet.
//!
//! Over a web page a notice runs edge to edge under the toolbar. Over the
//! settings or library page it is the first part of the page, so it keeps the
//! page's edges.

use ui::widget::{Banner, BannerKind, Container, Widget};

/// What a notice says.
#[derive(Clone, Debug)]
pub(crate) struct Notice {
    /// The id the notice's frame answers to, when something addresses it.
    pub id: Option<&'static str>,
    pub kind: BannerKind,
    pub title: String,
    pub message: Option<String>,
}

impl Notice {
    /// Something the person should know where nothing is wrong (a private
    /// window, paused history): primary ink, no hue.
    pub fn neutral(title: impl Into<String>) -> Self {
        Self::new(BannerKind::Neutral, title)
    }

    pub fn warning(title: impl Into<String>) -> Self {
        Self::new(BannerKind::Warning, title)
    }

    fn new(kind: BannerKind, title: impl Into<String>) -> Self {
        Self {
            id: None,
            kind,
            title: title.into(),
            message: None,
        }
    }

    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// The notice as a banner. `inset` frames it with the window's standard
    /// padding, for a row of its own under the toolbar; without it the banner
    /// takes the width of the page it is a part of.
    pub fn widget(&self, inset: bool) -> Box<dyn Widget> {
        let mut banner = Banner::new(self.kind, self.title.clone());
        if let Some(message) = &self.message {
            banner = banner.with_message(message.clone());
        }
        if let Some(id) = self.id {
            banner = banner.with_id(id);
        }
        if !inset {
            return Box::new(banner);
        }
        Box::new(Container::new().with_child(Box::new(banner)))
    }
}
