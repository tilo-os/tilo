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
    /// How many lines it takes: what does not fit is elided at the last of them, so a clamp
    /// holds however long the words are. Unset, it wraps to as many lines as its width needs.
    pub lines: Option<i64>,
    /// It elides at the room it is given rather than wrapping wider, so its box may shrink to
    /// nothing and what does not fit ends in an ellipsis: a title in a narrow card.
    pub truncate: bool,
}

impl Described for Text {
    type Props = TextProps;

    fn from_props(key: &str, props: TextProps) -> Self {
        let TextProps {
            value,
            role,
            numeric,
            secondary,
            lines,
            truncate,
        } = props;
        let mut text = Self::new("").with_role(role).with_id(key);
        if numeric {
            text = text.numeric();
        }
        if secondary {
            text = text.dimmed();
        }
        text = text
            .with_lines(lines.and_then(|lines| u32::try_from(lines).ok()))
            .with_truncate(truncate);
        text.show_content(value);
        text
    }

    /// What it says changes in place; another role, figure style or line limit builds it
    /// again.
    fn set_props(&mut self, props: TextProps) -> bool {
        let numeric = matches!(
            self.modifiers.kind,
            Some(crate::text_render::TextKind::Numeric)
        );
        let lines = props
            .lines
            .and_then(|lines| u32::try_from(lines).ok())
            .filter(|lines| *lines > 0);
        if props.role != self.role
            || props.numeric != numeric
            || props.secondary != self.dimmed
            || lines != self.max_lines()
            || props.truncate != self.truncates()
        {
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
