//! [`PictureChoice`] as a described component.

use crate::component::{Component, Described, Event, Value};
use crate::widget::Picture;

use super::{PictureChoice, PictureOption};
use crate::widget::picture as picture_module;

crate::component::part_slot!(PictureOptionProps);

/// A choice among pictures whose meaning is visual (a text size, a pointer size, a colour
/// filter), each tile showing its picture over its name, the chosen one edged.
#[derive(Component, Default)]
#[component(name = "PictureChoice", role = "Group", label = "label")]
pub struct PictureChoiceProps {
    /// The tiles, in order.
    #[component(slot(default))]
    pub options: Vec<PictureOptionProps>,
    /// Which tile is chosen, counted from 0: none until a person chooses one.
    pub selected: Option<i64>,
    /// Its spoken name.
    pub label: Option<String>,
    /// Whether it can be chosen from now.
    pub enabled: bool,
    /// A tile was chosen: its index, counted from 0, is the value.
    #[component(event(value = i64))]
    pub change: Event,
}

/// One tile of a [`PictureChoice`]: its picture and its name.
#[derive(Clone, Component, Default)]
#[component(name = "PictureOption", role = "RadioButton", label = "label")]
pub struct PictureOptionProps {
    /// What the tile says under its picture, and its spoken name.
    #[component(required, positional)]
    pub label: String,
    /// What the tile shows: a sample of the setting from the theme.
    #[component(required)]
    pub picture: PictureKind,
}

/// The picture a tile shows: one of the design system's own samples, the way a settings pane
/// shows what a choice means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PictureKind {
    /// "Aa" small, at the theme's own text size.
    TextSizeSmall,
    /// "Aa" regular.
    #[default]
    TextSizeRegular,
    /// "Aa" large.
    TextSizeLarge,
    /// The pointer at its smallest.
    PointerSmall,
    /// The pointer at its regular size.
    PointerRegular,
    /// The pointer at its largest.
    PointerLarge,
    /// The screen as it is.
    ColourFilterNone,
    /// The screen without hue or saturation, luminosity kept.
    ColourFilterGrayscale,
    /// The screen with every channel inverted.
    ColourFilterInverted,
    /// The screen grey, then inverted.
    ColourFilterGrayscaleInverted,
    /// A finger pressing the middle of a touchpad.
    TouchpadPress,
    /// A finger pressing the touchpad's bottom-right corner.
    TouchpadCornerPress,
    /// Two fingers pressing the touchpad together.
    TouchpadTwoFingerPress,
    /// Two fingers moving up and down together.
    TouchpadTwoFingerScroll,
    /// One finger moving along the touchpad's right edge.
    TouchpadEdgeScroll,
    /// A page moving with the fingers.
    ScrollDirectionNatural,
    /// A page moving against the fingers.
    ScrollDirectionReverse,
    /// Rows of a list at a compact density.
    DensityCompact,
    /// Rows of a list at a regular density.
    DensityRegular,
    /// Rows of a list at a comfortable density.
    DensityComfortable,
    /// The screen warmed, as night light tints it.
    WarmthWarm,
    /// The screen neither warm nor cool.
    WarmthNeutral,
    /// The screen cooled.
    WarmthCool,
    /// A notification banner with only its title.
    BannerTitle,
    /// A notification banner with a message under its title.
    BannerMessage,
}

crate::component::choice!(PictureKind, "PictureKind", {
    TextSizeSmall => "text_size_small",
    TextSizeRegular => "text_size_regular",
    TextSizeLarge => "text_size_large",
    PointerSmall => "pointer_small",
    PointerRegular => "pointer_regular",
    PointerLarge => "pointer_large",
    ColourFilterNone => "colour_filter_none",
    ColourFilterGrayscale => "colour_filter_grayscale",
    ColourFilterInverted => "colour_filter_inverted",
    ColourFilterGrayscaleInverted => "colour_filter_grayscale_inverted",
    TouchpadPress => "touchpad_press",
    TouchpadCornerPress => "touchpad_corner_press",
    TouchpadTwoFingerPress => "touchpad_two_finger_press",
    TouchpadTwoFingerScroll => "touchpad_two_finger_scroll",
    TouchpadEdgeScroll => "touchpad_edge_scroll",
    ScrollDirectionNatural => "scroll_direction_natural",
    ScrollDirectionReverse => "scroll_direction_reverse",
    DensityCompact => "density_compact",
    DensityRegular => "density_regular",
    DensityComfortable => "density_comfortable",
    WarmthWarm => "warmth_warm",
    WarmthNeutral => "warmth_neutral",
    WarmthCool => "warmth_cool",
    BannerTitle => "banner_title",
    BannerMessage => "banner_message",
});

impl From<PictureKind> for Picture {
    fn from(kind: PictureKind) -> Self {
        use picture_module::{ColourFilter, TouchpadGesture};
        match kind {
            PictureKind::TextSizeSmall => Picture::TextSize(0.8),
            PictureKind::TextSizeRegular => Picture::TextSize(1.0),
            PictureKind::TextSizeLarge => Picture::TextSize(1.4),
            PictureKind::PointerSmall => Picture::Pointer(16.0),
            PictureKind::PointerRegular => Picture::Pointer(24.0),
            PictureKind::PointerLarge => Picture::Pointer(32.0),
            PictureKind::ColourFilterNone => Picture::ColourFilter(ColourFilter::None),
            PictureKind::ColourFilterGrayscale => Picture::ColourFilter(ColourFilter::Grayscale),
            PictureKind::ColourFilterInverted => Picture::ColourFilter(ColourFilter::Inverted),
            PictureKind::ColourFilterGrayscaleInverted => {
                Picture::ColourFilter(ColourFilter::GrayscaleInverted)
            }
            PictureKind::TouchpadPress => Picture::Touchpad(TouchpadGesture::Press),
            PictureKind::TouchpadCornerPress => Picture::Touchpad(TouchpadGesture::CornerPress),
            PictureKind::TouchpadTwoFingerPress => {
                Picture::Touchpad(TouchpadGesture::TwoFingerPress)
            }
            PictureKind::TouchpadTwoFingerScroll => {
                Picture::Touchpad(TouchpadGesture::TwoFingerScroll)
            }
            PictureKind::TouchpadEdgeScroll => Picture::Touchpad(TouchpadGesture::EdgeScroll),
            PictureKind::ScrollDirectionNatural => Picture::ScrollDirection {
                natural: true,
                wheel: false,
            },
            PictureKind::ScrollDirectionReverse => Picture::ScrollDirection {
                natural: false,
                wheel: true,
            },
            PictureKind::DensityCompact => Picture::Density(0.85),
            PictureKind::DensityRegular => Picture::Density(1.0),
            PictureKind::DensityComfortable => Picture::Density(1.2),
            PictureKind::WarmthWarm => Picture::Warmth(2700),
            PictureKind::WarmthNeutral => Picture::Warmth(6500),
            PictureKind::WarmthCool => Picture::Warmth(9000),
            PictureKind::BannerTitle => Picture::Banner { message: false },
            PictureKind::BannerMessage => Picture::Banner { message: true },
        }
    }
}

impl From<PictureOptionProps> for PictureOption {
    fn from(props: PictureOptionProps) -> Self {
        Self::new(props.label, props.picture.into())
    }
}

impl Described for PictureChoice {
    type Props = PictureChoiceProps;

    fn from_props(key: &str, props: PictureChoiceProps) -> Self {
        let PictureChoiceProps {
            options,
            selected,
            label,
            enabled,
            change,
        } = props;
        let options: Vec<PictureOption> = options.into_iter().map(PictureOption::from).collect();
        let count = options.len();
        let mut choice = Self::new(
            options,
            selected.map(|index| {
                usize::try_from(index.max(0))
                    .unwrap_or_default()
                    .min(count.saturating_sub(1))
            }),
        )
        .with_id(key)
        .with_enabled(enabled);
        if let Some(label) = label {
            choice = choice.with_accessible_name(label);
        }
        if !change.is_none() {
            choice = choice.on_change(move |index| {
                change.fire(Some(Value::Int(i64::try_from(index).unwrap_or(i64::MAX))));
            });
        }
        choice
    }

    /// What is chosen changes in place; other tiles are built again.
    fn set_props(&mut self, props: PictureChoiceProps) -> bool {
        if props.options.len() != self.count() {
            return false;
        }
        self.set_selected(
            props
                .selected
                .and_then(|index| usize::try_from(index).ok())
                .filter(|index| *index < self.count()),
        );
        self.set_enabled(props.enabled);
        true
    }
}
