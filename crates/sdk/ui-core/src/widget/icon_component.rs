//! [`IconWidget`] as a described component.

use crate::component::{Component, Described};
use crate::icons::Icon;
use crate::widget::{IconSize, Tone};

use super::IconWidget;

crate::component::choice!(IconSize, "IconSize", {
    Small => "small",
    Regular => "regular",
    Large => "large",
});

/// One glyph from the icon set, in the theme's ink.
#[derive(Component)]
#[component(name = "Icon", role = "Image")]
pub struct IconProps {
    /// The glyph.
    #[component(required, positional)]
    pub icon: Icon,
    /// How large it is: `regular` unless said, `small` beside a caption, `large` beside a title.
    pub size: Option<IconSize>,
    /// Recede beside its label, in secondary ink.
    pub secondary: bool,
    /// Take the accent's ink, for the one glyph that marks what is chosen.
    pub accent: bool,
    /// A status the glyph means, in that status's colour: a passed check, a warning sign.
    pub tone: Option<Tone>,
    /// What the glyph means, where it stands without words beside it.
    pub description: Option<String>,
}

impl Default for IconProps {
    fn default() -> Self {
        Self {
            icon: Icon::Info,
            size: None,
            secondary: false,
            accent: false,
            tone: None,
            description: None,
        }
    }
}

impl Described for IconWidget {
    type Props = IconProps;

    fn from_props(_key: &str, props: IconProps) -> Self {
        let IconProps {
            icon,
            size,
            secondary,
            accent,
            tone,
            description,
        } = props;
        let widget = Self::new(icon).sized(size.unwrap_or_default());
        let widget = match (tone, accent, secondary) {
            (Some(tone), _, _) => widget.tone(tone),
            (None, true, _) => widget.accent(),
            (None, false, true) => widget.dim(),
            (None, false, false) => widget,
        };
        match description.filter(|it| !it.trim().is_empty()) {
            Some(it) => widget.describing(it),
            None => widget,
        }
    }

    fn set_props(&mut self, props: IconProps) -> bool {
        let tint = match (props.tone, props.accent, props.secondary) {
            (Some(tone), _, _) => super::Tint::Tone(tone),
            (None, true, _) => super::Tint::Accent,
            (None, false, true) => super::Tint::Dim,
            (None, false, false) => super::Tint::Primary,
        };
        self.set_glyph(props.icon, props.size.unwrap_or_default(), tint);
        self.set_description(props.description.filter(|it| !it.trim().is_empty()));
        true
    }
}
