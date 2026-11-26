//! The window's places: the overview and the sub-agents the main agent started, with the
//! person the assistant works for at the foot.

use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::{ListRow, Tone};

use crate::sources::{Progress, Read, SubAgent};
use crate::{Hands, Nav, Place, tr};

fn sub_agent(sub: &SubAgent) -> NavItem {
    let (progress, tone) = match &sub.progress {
        Progress::Working => (tr("assistant-progress-working"), Tone::Info),
        Progress::NeedsYou => (tr("assistant-progress-needs-you"), Tone::Warning),
        Progress::Done => (tr("assistant-progress-done"), Tone::Success),
        Progress::Failed => (tr("assistant-progress-failed"), Tone::Danger),
        Progress::Stopped => (tr("assistant-progress-stopped"), Tone::Neutral),
    };
    NavItem::new(Place::SubAgent(sub.id.clone()).key(), sub.job.clone())
        .secondary(sub.role.clone())
        .status_label(progress, tone)
}

pub(crate) fn view(nav: &Nav, hands: &Hands) -> Element {
    let main = NavItem::new(Place::Overview.key(), tr("assistant-overview"))
        .icon(Icon::Gauge)
        .semantic_key("assistant.overview");
    let review = NavItem::new(Place::Proposals.key(), tr("assistant-review-proposals"))
        .resource(Place::PROPOSALS)
        .icon(Icon::ListChecks)
        .semantic_key("assistant.proposals");
    let (listed, none) = match hands.reads.sub_agents() {
        Read::Here(listed) => (
            listed.iter().map(sub_agent).collect(),
            tr("assistant-sub-agents-none"),
        ),
        Read::NotHere => (Vec::new(), tr("assistant-agents-not-here")),
    };
    let sub_agents =
        NavSection::new("sub-agents", tr("assistant-sub-agents"), listed).empty_caption(none);
    let chosen = nav.clone();
    let places = NavList::new(vec![
        NavSection::untitled("overview", vec![main, review]),
        sub_agents,
    ])
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
