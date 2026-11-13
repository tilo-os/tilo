//! What a row leads with: the builders that fill its leading column.

use super::{Leading, ListRow};
use crate::icons::Icon;
use crate::widget::{MediaAspect, MediaSource};
use vello::peniko::{Color, ImageData};

impl ListRow {
    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.leading = Some(Leading::Icon(icon));
        self
    }

    /// See [`RowLeading::Badge`](super::RowLeading::Badge).
    #[must_use]
    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.leading = Some(Leading::Badge(badge.into()));
        self
    }

    /// What the leading badge or icon stands for ("Retrieves the resource" for `GET`, "SSH"
    /// for a terminal glyph): the tooltip when the pointer rests on it, and the row's
    /// accessible description.
    #[must_use]
    pub fn leading_tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.leading_tooltip = Some(tooltip.into());
        self
    }

    /// See [`RowLeading::Thumbnail`](super::RowLeading::Thumbnail).
    #[must_use]
    pub fn thumbnail(mut self, image: ImageData) -> Self {
        self.leading = Some(Leading::Thumbnail(image));
        self
    }

    /// See [`RowLeading::Media`](super::RowLeading::Media): art from `source` at `aspect`,
    /// the placeholder with the title's initials until it loads.
    #[must_use]
    pub fn media(mut self, source: MediaSource, aspect: MediaAspect) -> Self {
        self.leading = Some(Leading::Media {
            source,
            aspect,
            loaded: None,
        });
        self
    }

    /// See [`RowLeading::Checkbox`](super::RowLeading::Checkbox): a selection checkbox.
    #[must_use]
    pub fn checkbox(self, checked: bool, on_toggle: impl FnMut(bool) + 'static) -> Self {
        self.checked_with(checked, None, on_toggle)
    }

    /// [`Self::checkbox`] with a glyph after the box: a task checks off and still says how
    /// important it is.
    #[must_use]
    pub fn checkbox_with_icon(
        self,
        checked: bool,
        icon: Icon,
        on_toggle: impl FnMut(bool) + 'static,
    ) -> Self {
        self.checked_with(checked, Some(icon), on_toggle)
    }

    fn checked_with(
        mut self,
        checked: bool,
        icon: Option<Icon>,
        on_toggle: impl FnMut(bool) + 'static,
    ) -> Self {
        self.leading = Some(Leading::Checkbox {
            checked,
            on_toggle: Box::new(on_toggle),
            icon,
        });
        self
    }

    /// See [`RowLeading::Swatch`](super::RowLeading::Swatch).
    #[must_use]
    pub fn swatch(mut self, colour: Color) -> Self {
        self.leading = Some(Leading::Swatch(colour));
        self
    }

    /// See [`RowLeading::Signal`](super::RowLeading::Signal): the row leads with how strong its
    /// signal is, and is described by the meter's name ("Strong signal")
    /// unless a tooltip already says more.
    #[must_use]
    pub fn signal(mut self, level: crate::widget::SignalLevel) -> Self {
        self.leading = Some(Leading::Signal(level));
        if self.leading_tooltip.is_none() {
            self.leading_tooltip = Some(level.name());
        }
        self
    }
}
