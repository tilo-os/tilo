//! [`Text`] as a described component.

use crate::component::{Component, Described};
use crate::tokens::TextRole;

use super::{Text, TextSource};

/// Words on the page, set in one of the design system's text roles. What it says is content,
/// never looked up as interface copy.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "Text", role = "Label", label = "value")]
pub struct TextProps {
    /// What it says.
    #[component(required, positional)]
    pub value: String,
    /// What the text is, which sets its size, weight and face: `body` unless said.
    pub role: TextRole,
    /// A number that changes: tabular figures, so it does not jitter as it changes.
    pub numeric: bool,
    /// What it says stands beside the content it is about, in secondary ink: "edited" after a
    /// message, "Pinned" over it.
    pub secondary: bool,
}

impl Described for Text {
    type Props = TextProps;

    fn from_props(key: &str, props: TextProps) -> Self {
        let TextProps {
            value,
            role,
            numeric,
            secondary,
        } = props;
        let mut text = Self::new("").with_role(role).with_id(key);
        if numeric {
            text = text.numeric();
        }
        if secondary {
            text = text.dimmed();
        }
        text.show_content(value);
        text
    }

    /// What it says changes in place; another role or figure style builds it again.
    fn set_props(&mut self, props: TextProps) -> bool {
        let numeric = matches!(
            self.modifiers.kind,
            Some(crate::text_render::TextKind::Numeric)
        );
        if props.role != self.role || props.numeric != numeric || props.secondary != self.dimmed {
            return false;
        }
        self.show_content(props.value);
        true
    }
}

impl Text {
    /// Show `value` as content: it is never taken for a message of the app's catalogue.
    pub(crate) fn show_content(&mut self, value: String) {
        if self.source.is_none() {
            self.source = Some(TextSource::Local(Box::new(|| None)));
        }
        if self.label != value {
            self.label = value;
            self.dirty = true;
            self.layout_dirty = true;
            self.prepared_input = None;
        }
    }
}
