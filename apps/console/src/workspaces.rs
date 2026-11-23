//! Workspaces: the person's workspaces the MCP server reaches, whether they made them or an agent
//! did, each one step from the full desktop.

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::mcp_server;
use ui::widget::{ListRow, Section};

use crate::{Console, Reads, Resources, ago, page, tr, trf};

pub(crate) fn view(cx: &ViewCx<Console>, reads: &Reads) -> Element {
    let records = Resources::of(cx);
    let read = &reads.server;
    Box::new(cx.arriving(&read.answer(), move |_, server| {
        if server.workspaces.is_empty() {
            return page(vec![Box::new(
                EmptyState::new(tr("console-workspaces-empty"))
                    .with_message(tr("console-workspaces-empty-message"))
                    .with_icon(Icon::LayoutGrid)
                    .with_id("console-workspaces-empty"),
            )]);
        }
        let rows = server
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, workspace)| row(&records, index, workspace))
            .collect();
        page(vec![Box::new(Section::new(rows))])
    }))
}

fn row(records: &Resources, index: usize, workspace: &mcp_server::Workspace) -> Element {
    let by = match &workspace.made_by {
        Some(mcp_server::Maker::Person) => Some(tr("console-workspace-made-by-you")),
        Some(mcp_server::Maker::Agent { agent, .. }) => Some(trf(
            "console-workspace-made-by",
            &[("agent", agent.clone().into())],
        )),
        None => None,
    };
    let (records, href) = (records.clone(), workspace.open.clone());
    let open = Button::new(
        &format!("console-workspace-{index}-open"),
        "console-workspace-open",
    )
    .with_icon(Icon::ExternalLink)
    .on_open(move || records.open_in_tilo(&href));
    let mut row = ListRow::new(format!("console-workspace-{index}"), workspace.name.clone())
        .icon(Icon::LayoutGrid)
        .meta(ago(workspace.made_ms))
        .trailing(Box::new(open));
    if let Some(by) = by {
        row = row.subtitle(by);
    }
    Box::new(row)
}
