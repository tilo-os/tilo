//! Workspaces: the person's workspaces the door reaches, whether they made them or an agent
//! did, each one step from the full desktop.

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::agent_door as door;
use ui::widget::{ListRow, Section};

use crate::{Console, Doors, Reads, ago, page, tr, trf};

pub(crate) fn view(cx: &ViewCx<Console>, reads: &Reads) -> Element {
    let doors = Doors::of(cx);
    let read = &reads.door;
    Box::new(cx.arriving(&read.answer(), move |_, door| {
        if door.workspaces.is_empty() {
            return page(vec![Box::new(
                EmptyState::new(tr("console-workspaces-empty"))
                    .with_message(tr("console-workspaces-empty-message"))
                    .with_icon(Icon::LayoutGrid)
                    .with_id("console-workspaces-empty"),
            )]);
        }
        let rows = door
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, workspace)| row(&doors, index, workspace))
            .collect();
        page(vec![Box::new(Section::new(rows))])
    }))
}

fn row(doors: &Doors, index: usize, workspace: &door::Workspace) -> Element {
    let by = match &workspace.made_by {
        Some(door::Maker::Person) => Some(tr("console-workspace-made-by-you")),
        Some(door::Maker::Agent { agent, .. }) => Some(trf(
            "console-workspace-made-by",
            &[("agent", agent.clone().into())],
        )),
        None => None,
    };
    let (doors, href) = (doors.clone(), workspace.open.clone());
    let open = Button::new(
        &format!("console-workspace-{index}-open"),
        "console-workspace-open",
    )
    .with_icon(Icon::ExternalLink)
    .on_view(move || doors.open_in_tilo(&href));
    let mut row = ListRow::new(format!("console-workspace-{index}"), workspace.name.clone())
        .icon(Icon::LayoutGrid)
        .meta(ago(workspace.made_ms))
        .trailing(Box::new(open));
    if let Some(by) = by {
        row = row.subtitle(by);
    }
    Box::new(row)
}
