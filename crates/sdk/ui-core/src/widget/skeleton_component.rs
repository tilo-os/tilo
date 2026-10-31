//! [`Skeleton`] as a described component.

use crate::component::{Component, Described};
use crate::widget::{MediaAspect, RowLeadingShape, RowShape};

use super::{Skeleton, SkeletonShape};

/// What kind of content a skeleton stands in for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkeletonForm {
    /// Lines of body text.
    #[default]
    Text,
    /// A list row.
    Row,
    /// A media card's art and title.
    Media,
    /// A status chip.
    Chip,
    /// A whole page, filling the space it is given.
    Page,
}

crate::component::choice!(SkeletonForm, "SkeletonForm", {
    Text => "text",
    Row => "row",
    Media => "media",
    Chip => "chip",
    Page => "page",
});

/// The shape of content still on its way, drawn where it will stand and at its size, so nothing
/// moves when it arrives. It never shimmers, and it says "Loading" to assistive technology.
#[derive(Component, Default)]
#[component(name = "Skeleton", role = "Generic")]
pub struct SkeletonProps {
    /// What it stands in for: `text` unless said.
    #[component(positional)]
    pub shape: Option<SkeletonForm>,
    /// How many lines: of text (3 unless said), or under a row's title (none unless said).
    pub lines: Option<i64>,
    /// What leads a row: a mark, an avatar or a tile.
    pub leading: Option<RowLeadingShape>,
    /// The shape of a media card's art: `square` unless said.
    pub aspect: Option<MediaAspect>,
}

impl SkeletonProps {
    fn shape(&self) -> SkeletonShape {
        let lines = |default: u8| {
            self.lines
                .map_or(default, |n| u8::try_from(n.clamp(0, 12)).unwrap_or(default))
        };
        match self.shape.unwrap_or_default() {
            SkeletonForm::Text => SkeletonShape::Text {
                lines: lines(3).max(1),
            },
            SkeletonForm::Row => SkeletonShape::Row(
                RowShape::TITLE
                    .with_secondary(lines(0))
                    .led_by(self.leading.unwrap_or_default()),
            ),
            SkeletonForm::Media => SkeletonShape::Media(self.aspect.unwrap_or_default()),
            SkeletonForm::Chip => SkeletonShape::Chip,
            SkeletonForm::Page => SkeletonShape::Page,
        }
    }
}

impl Described for Skeleton {
    type Props = SkeletonProps;

    fn from_props(_key: &str, props: SkeletonProps) -> Self {
        Self::new(props.shape())
    }
}
