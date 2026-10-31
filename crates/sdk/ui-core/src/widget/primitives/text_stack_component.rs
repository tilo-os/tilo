//! [`TextStack`] as a described component.

use crate::component::{Component, Described};
use crate::widget::Tone;

use super::{StackSize, TextStack};

/// A title over a secondary line, with an optional third line under it, in fixed roles and at
/// fixed gaps: a file's name over its folder, a person over their role. Each line is one line,
/// cut short with an ellipsis; two facts never share a line.
#[derive(Component, Default)]
#[component(name = "TextStack", role = "Group", label = "title")]
pub struct TextStackProps {
    /// The first line, in the body's medium weight.
    #[component(required, positional)]
    pub title: String,
    /// The line under the title, in secondary ink.
    pub secondary: Option<String>,
    /// A third line, in tertiary ink.
    pub tertiary: Option<String>,
    /// A status the secondary line means, in its colour: "Failed" in danger.
    pub tone: Option<Tone>,
    /// A dense row's or a bar's stack: a smaller title.
    pub dense: bool,
}

impl Described for TextStack {
    type Props = TextStackProps;

    fn from_props(_key: &str, props: TextStackProps) -> Self {
        let TextStackProps {
            title,
            secondary,
            tertiary,
            tone,
            dense,
        } = props;
        let mut stack = Self::new(title)
            .secondary(secondary.unwrap_or_default())
            .tertiary(tertiary.unwrap_or_default());
        if dense {
            stack = stack.dense();
        }
        stack.secondary_tone = tone.unwrap_or_default();
        stack
    }

    /// Its lines change in place.
    fn set_props(&mut self, props: TextStackProps) -> bool {
        let size = if props.dense {
            StackSize::Dense
        } else {
            StackSize::Regular
        };
        if size != self.size {
            return false;
        }
        self.set_lines(
            &props.title,
            props.secondary.as_deref().unwrap_or_default(),
            props.tertiary.as_deref().unwrap_or_default(),
        );
        self.set_secondary_tone(props.tone.unwrap_or_default());
        true
    }
}
