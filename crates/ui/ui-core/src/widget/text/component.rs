//! [`Text`] as a described component.

use tilo_ui_base::text_render::ParagraphAlign;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::{component::Component, component::Described};

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
    /// It elides at the width it is given rather than wrapping wider, so its box may shrink to
    /// nothing and what does not fit ends in an ellipsis: a title in a narrow card.
    pub truncate: bool,
    /// What it says is announced when it changes instead of only read on the way past: a line a
    /// person is told without looking (a notice passing, a status that moved). `polite` waits for
    /// a pause, `assertive` interrupts; unset, it is read without announcing.
    pub live: Option<tilo_ui_base::a11y::Politeness>,
    /// Where its lines sit across the width it is given: `start` unless said, `center` for a
    /// short message standing alone in the middle of a page.
    pub align: ParagraphAlign,
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
            live,
            align,
        } = props;
        let mut text = Self::new("").with_role(role).with_id(key);
        if align != ParagraphAlign::Start {
            text = text.with_align(align);
        }
        if numeric {
            text = text.numeric();
        }
        if secondary {
            text = text.dimmed();
        }
        if let Some(live) = live {
            text = text.with_live(live);
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
            Some(tilo_ui_base::text_render::TextKind::Numeric)
        );
        let lines = props
            .lines
            .and_then(|lines| u32::try_from(lines).ok())
            .filter(|lines| *lines > 0);
        if props.role != self.role
            || props.numeric != numeric
            || props.secondary != self.dimmed
            || props.live != self.live
            || lines != self.max_lines()
            || props.truncate != self.truncates()
            || props.align != self.modifiers.layered.align
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

#[cfg(test)]
mod tests {
    use super::{Text, TextProps};
    use tilo_ui_base::component::Described;
    use tilo_ui_base::text_render::ParagraphAlign;

    fn text(align: ParagraphAlign) -> Text {
        Text::from_props(
            "message",
            TextProps {
                value: "More formats are installed".to_owned(),
                align,
                ..TextProps::default()
            },
        )
    }

    #[test]
    fn centred_words_stay_centred_and_a_new_alignment_lays_them_out_again() {
        let mut centred = text(ParagraphAlign::Center);
        assert_eq!(centred.modifiers.layered.align, ParagraphAlign::Center);
        assert!(centred.set_props(TextProps {
            value: "Could not install more formats".to_owned(),
            align: ParagraphAlign::Center,
            ..TextProps::default()
        }));
        assert!(
            !centred.set_props(TextProps {
                value: "Could not install more formats".to_owned(),
                ..TextProps::default()
            }),
            "moving the words to the start builds the text again"
        );
        assert_eq!(
            text(ParagraphAlign::Start).modifiers.layered.align,
            ParagraphAlign::Start
        );
    }
}
