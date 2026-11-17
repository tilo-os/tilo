//! The window's places: the main chat and the sub-agents the main agent started, with the
//! person the assistant works for at the foot.

use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::{ListRow, Tone};

use crate::sources::{Read, Standing, SubAgent};
use crate::{Hands, Nav, Place, tr, trf};

fn sub_agent(sub: &SubAgent) -> NavItem {
    let (standing, tone) = match &sub.standing {
        Standing::Working => (tr("assistant-standing-working"), Tone::Info),
        Standing::NeedsYou => (tr("assistant-standing-needs-you"), Tone::Warning),
        Standing::Done { when } => (
            trf("assistant-standing-done", &[("when", when.clone().into())]),
            Tone::Success,
        ),
    };
    NavItem::new(Place::SubAgent(sub.id.clone()).key(), sub.job.clone())
        .secondary(sub.role.clone())
        .status_label(standing, tone)
}

pub(crate) fn view(nav: &Nav, hands: &Hands) -> Element {
    let main = NavItem::new(Place::MainChat.key(), tr("assistant-main-chat"))
        .icon(Icon::MessageCircle)
        .semantic_key("assistant.main-chat");
    let (listed, none) = match hands.reads.sub_agents() {
        Read::Here(listed) => (
            listed.iter().map(sub_agent).collect(),
            tr("assistant-sub-agents-none"),
        ),
        Read::NotHere => (Vec::new(), tr("assistant-not-here")),
    };
    let sub_agents =
        NavSection::new("sub-agents", tr("assistant-sub-agents"), listed).empty_caption(none);
    let chosen = nav.clone();
    let places = NavList::new(vec![NavSection::untitled("chat", vec![main]), sub_agents])
        .with_accessible_name(tr("assistant-places"))
        .with_semantic_key("assistant.places")
        .bind_selection(&nav.place)
        .on_view("assistant.places", move |_| {
            chosen.asking.set(false);
            chosen.pane.set(ui::widget::NavPane::Content);
        });
    let mut column = Column::new().with_no_padding().push(Box::new(
        Column::new()
            .with_no_padding()
            .with_fill_remaining_height()
            .push(Box::new(places)),
    ));
    if let Some((name, identity)) = &hands.person {
        column = column.push(Box::new(
            ListRow::new("assistant-person", name.clone()).avatar(name.clone(), identity.clone()),
        ));
    }
    Box::new(column)
}
