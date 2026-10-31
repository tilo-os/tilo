//! [`ConversationList`] as a described component: the lines of a conversation as parts, each
//! what it says, who said it and when, and how it is on its way.
//!
//! The list is static — it paints and it measures, takes no pointer, holds no selection and
//! keeps no state of its own — so it reports no events: the day's dividers, the runs of one
//! author's lines, the clock on each line and the delivery state under your own are the list's
//! own reading of the lines it is given. The lines change in place: one that arrived or changed
//! is taken through [`Described`]'s `set_props`, and the list measures only what it changed.

use crate::component::{Component, Described, Moment, ResourceId};
use crate::widget::Widget;
use crate::widget::conversation::{ConversationLine, ConversationList, DeliveryState};

crate::component::choice!(crate::widget::conversation::DeliveryState, "Delivery", {
    Queued => "queued",
    Sending => "sending",
    Submitted => "submitted",
    Sent => "sent",
    Delivered => "delivered",
    Read => "read",
    Failed => "failed",
    Uncertain => "uncertain",
});

/// One line of a conversation: who said it and when, what it says, and how far on its way it is.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(
    name = "ConversationEntry",
    role = "ListItem",
    label = "said",
    presents = "item"
)]
pub struct ConversationEntryProps {
    /// The message it stands for: its stable identity, what a caller anchors a scroll to, and
    /// what changes follow.
    #[component(required, positional)]
    pub item: ResourceId,
    /// Who said it, as the person reads names. Empty for one that knowingly continues the run
    /// above it.
    pub author: Option<String>,
    /// When it was said: a divider marks the first line of each day, and the time shows beside
    /// an author's name and under your own line.
    pub time: Option<Moment>,
    /// What it says.
    #[component(required, positional)]
    pub said: String,
    /// Your own line (on the right, with its delivery state under it) rather than the other
    /// person's (on the left, under their name).
    pub outgoing: bool,
    /// How far your own line is on its way.
    pub delivery: Option<DeliveryState>,
}

crate::component::part_slot!(ConversationEntryProps);

/// A conversation, drawn as every conversation is drawn: each line on its own side, runs of one
/// person's consecutive messages under their name once, a divider where the day changes, and the
/// time and the delivery state under your own.
#[derive(Component, Default)]
#[component(name = "ConversationList", role = "List", label = "name")]
pub struct ConversationListProps {
    /// What the conversation is, for assistive technology: "Conversation with Ada".
    pub name: Option<String>,
    /// The lines, oldest first.
    #[component(slot(default))]
    pub lines: Vec<ConversationEntryProps>,
}

/// The line one entry makes, as the list draws it.
fn line_of(entry: &ConversationEntryProps) -> ConversationLine {
    let at_ms = entry
        .time
        .map_or(0, |moment| moment.0.saturating_mul(1_000));
    let key = entry.item.0.clone();
    let mut line = if entry.outgoing {
        ConversationLine::outgoing(key, at_ms, entry.said.clone())
    } else {
        ConversationLine::incoming(
            key,
            at_ms,
            entry.author.clone().unwrap_or_default(),
            entry.said.clone(),
        )
    };

    if let Some(author) = entry.author.clone().filter(|author| !author.is_empty()) {
        line = line.with_author(author);
    }
    if let Some(delivery) = entry.delivery {
        line = line.with_delivery(delivery);
    }
    line
}

/// A described conversation: its list, and the lines it was made with.
pub struct DescribedConversationList {
    list: ConversationList,
    lines: Vec<ConversationEntryProps>,
    name: Option<String>,
}

impl DescribedConversationList {
    /// The list, for a test of what it draws.
    #[must_use]
    pub fn list(&self) -> &ConversationList {
        &self.list
    }

    /// The list, to change.
    pub fn list_mut(&mut self) -> &mut ConversationList {
        &mut self.list
    }

    /// The lines it was made with.
    #[must_use]
    pub fn entries(&self) -> &[ConversationEntryProps] {
        &self.lines
    }
}

impl Described for DescribedConversationList {
    type Props = ConversationListProps;

    fn from_props(_key: &str, props: ConversationListProps) -> Self {
        let ConversationListProps { name, lines } = props;
        let mut list = ConversationList::new(lines.iter().map(line_of).collect());
        if let Some(name) = &name {
            list = list.with_accessible_name(name.clone());
        }
        Self { list, lines, name }
    }

    /// Changed lines are taken in place, and the list measures only what changed. A changed name
    /// builds the list again, since the name is the list's for assistive technology.
    fn set_props(&mut self, props: ConversationListProps) -> bool {
        let ConversationListProps { name, lines } = props;
        if name != self.name {
            return false;
        }
        if lines == self.lines {
            return true;
        }
        self.list.set_lines(lines.iter().map(line_of).collect());
        self.lines = lines;
        true
    }
}

/// A conversation is its list.
impl crate::widget::Decorator for DescribedConversationList {
    type Inner = ConversationList;

    fn inner(&self) -> &ConversationList {
        &self.list
    }

    fn inner_mut(&mut self) -> &mut ConversationList {
        &mut self.list
    }

    fn name(&self) -> &'static str {
        "ConversationList"
    }

    /// The list's change is the described conversation's: it adds no structure of its own.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.list.take_structure_change()
    }
}
