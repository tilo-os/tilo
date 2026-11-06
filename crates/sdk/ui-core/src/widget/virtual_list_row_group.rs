//! What a row's group is: the heading a run of rows stands under, and the tone it reads in.

use crate::widget::Tone;

/// The row's place in a group of the list ([`super::ListModel::group_at`]): the heading the run
/// of rows stands under, and the tail of items done with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowGroup {
    /// Rows whose group reads this heading stand together under it, in the order they come,
    /// the heading and its count in the tone the group reads in: a day gone by says it is
    /// overdue in the danger tone, and a day a thing was done reads quietly in no tone of its
    /// own.
    Heading { text: String, tone: Tone },
    /// The items done with: the model stands them last, after every heading group, and the
    /// list draws them under its tail title ([`super::VirtualList::with_tail_title`]), closed
    /// until the person opens them.
    Tail,
}

impl RowGroup {
    /// A heading read in the neutral tone: a group that says nothing more than what it holds.
    #[must_use]
    pub fn heading(text: impl Into<String>) -> Self {
        Self::Heading {
            text: text.into(),
            tone: Tone::Neutral,
        }
    }
}
