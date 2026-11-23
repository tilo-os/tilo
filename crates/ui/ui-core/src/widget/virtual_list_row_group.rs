//! What a row's group is: the heading a run of rows stands under, and the tone it reads in.

use crate::widget::Tone;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowGroup {
    Heading { text: String, tone: Tone },
    Tail,
}

impl RowGroup {
    #[must_use]
    pub fn heading(text: impl Into<String>) -> Self {
        Self::Heading {
            text: text.into(),
            tone: Tone::Neutral,
        }
    }
}
