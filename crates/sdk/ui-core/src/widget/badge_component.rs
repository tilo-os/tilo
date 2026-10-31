//! [`Badge`] as a described component.

use crate::component::{Component, Described};
use crate::widget::Tone;
use crate::widget::chip::ChipStyle;

use super::{Badge, BadgeContent};

/// A small pill on another element: how many new things there are, a dot for something new, a
/// short status ("Owner", "Beta"), or the mark of unsaved changes. It is never pressed.
#[derive(Component, Default)]
#[component(name = "Badge", role = "StaticText", label = "text")]
pub struct BadgeProps {
    /// A short status in a word: "Owner", "Beta".
    #[component(positional)]
    pub text: Option<String>,
    /// How many new things there are, in your colour: nothing at 0, "99+" past 99.
    pub count: Option<i64>,
    /// Something new, without a number: a dot in your colour.
    pub dot: bool,
    /// Unsaved changes: a dot in the primary ink, which is nobody's and not new.
    pub modified: bool,
    /// A status the pill means, washed into it. A text badge is neutral unless said.
    pub tone: Option<Tone>,
}

impl BadgeProps {
    /// What the badge shows, and how.
    fn content_and_style(&self) -> (BadgeContent, ChipStyle) {
        let content = if self.modified {
            BadgeContent::Modified
        } else if self.dot {
            BadgeContent::Dot
        } else if let Some(count) = self.count {
            BadgeContent::Count(u32::try_from(count.max(0)).unwrap_or(u32::MAX))
        } else {
            BadgeContent::Text(self.text.clone().unwrap_or_default())
        };
        let style = match (self.tone, &content) {
            (Some(tone), _) => ChipStyle::Tinted(tone),
            (None, BadgeContent::Dot | BadgeContent::Count(_)) => ChipStyle::Live,
            (None, BadgeContent::Modified | BadgeContent::Text(_)) => ChipStyle::Neutral,
        };
        (content, style)
    }
}

impl Described for Badge {
    type Props = BadgeProps;

    fn from_props(_key: &str, props: BadgeProps) -> Self {
        let (content, style) = props.content_and_style();
        Self::with_content(content, style)
    }

    /// A changed count cross-fades in place, as [`Badge::set_count`] does.
    fn set_props(&mut self, props: BadgeProps) -> bool {
        let (content, style) = props.content_and_style();
        if style != self.style {
            self.style = style;
            self.dirty = true;
        }
        self.set_content(content);
        true
    }
}
