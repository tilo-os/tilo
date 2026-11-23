//! Connect: where an MCP client reaches the person's workspaces, and every
//! agent the MCP server let in, with Revoke.

use ui::framework::ReadFailure;
use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::mcp_server;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::widget::{ButtonRole, ChipStyle, ListRow, Property, PropertyList, Section};

use crate::{Console, Reads, Resources, ago, faced, page, tr, trf};

pub(crate) fn view(cx: &ViewCx<Console>, reads: &Reads) -> Element {
    let resources = Resources::of(cx);
    let answer = reads.server.answer();
    // A server that keeps no MCP grants for this person (someone else's, which a page in a
    // workspace there reaches) answers no read of them: their agent connects by setting Tilo up
    // on its computer and signing in as them, the setup their own card shows.
    cx.switch(&answer.state(), move |cx, read| match read {
        Arrival::Failed(_) => set_up(&resources),
        Arrival::Pending { .. } | Arrival::Ready(_) => connected(cx, &resources, &answer),
    })
}

/// Setting Tilo up on the computer of the person's agent, which signs it in as them.
fn set_up(resources: &Resources) -> Element {
    let resources = resources.clone();
    page(vec![
        Box::new(Text::new(&tr("console-connect-setup-lead"))),
        Box::new(
            Button::new("console-connect-setup", "console-connect-setup")
                .role(ButtonRole::Primary)
                .on_open(move || resources.show_connect()),
        ),
    ])
}

/// Where an agent reaches the MCP server, and every agent it let in.
fn connected(
    cx: &ViewCx<Console>,
    resources: &Resources,
    answer: &Arriving<mcp_server::Server, ReadFailure>,
) -> Element {
    let resources = resources.clone();
    Box::new(cx.arriving(answer, move |_, server| {
        let commands = [
            ("console-client-url", server.endpoint.clone()),
            (
                "console-client-other",
                mcp_server::client_config(&server.endpoint),
            ),
        ]
        .into_iter()
        .map(|(label, value)| {
            Property::new(tr(label), value)
                .with_kind(ui::text_render::TextKind::Code)
                .copyable()
        })
        .collect();
        let mut parts: Vec<Element> = vec![
            Box::new(Text::new(&tr("console-connect-lead"))),
            Box::new(Section::titled(
                tr("console-connect-heading"),
                vec![Box::new(
                    PropertyList::new(commands).with_id("console-connect-commands"),
                )],
            )),
        ];
        let agents: Vec<Element> = if server.grants.is_empty() {
            vec![Box::new(
                EmptyState::new(tr("console-grants-empty"))
                    .with_icon(Icon::Link)
                    .with_id("console-grants-empty"),
            )]
        } else {
            server
                .grants
                .iter()
                .enumerate()
                .map(|(index, grant)| grant_row(&resources, index, grant))
                .collect()
        };
        parts.push(Box::new(Section::titled(
            tr("console-grants-heading"),
            agents,
        )));
        page(parts)
    }))
}

/// Where the grant lets the agent work, in words.
pub(crate) fn access(grant: &mcp_server::Grant) -> String {
    match &grant.reach {
        mcp_server::Reach::Account => tr("console-reach-account"),
        mcp_server::Reach::Workspace { name, .. } => trf(
            "console-reach-workspace",
            &[("workspace", name.clone().into())],
        ),
    }
}

fn grant_row(records: &Resources, index: usize, grant: &mcp_server::Grant) -> Element {
    let agent = wire::Author {
        name: grant.agent.clone(),
        relation: wire::Relation::Software,
        ..wire::Author::default()
    };
    let status = if grant.sessions.is_empty() {
        grant.last_ms.map_or_else(
            || {
                trf(
                    "console-grant-granted",
                    &[("when", ago(grant.granted_ms).into())],
                )
            },
            |last| trf("console-grant-last-active", &[("when", ago(last).into())]),
        )
    } else {
        trf(
            "console-grant-running",
            &[("count", grant.sessions.len().into())],
        )
    };
    let revoke = {
        let (records, id) = (records.clone(), grant.id.clone());
        Button::new(
            &format!("console-grant-{index}-revoke"),
            "console-grant-revoke",
        )
        .role(ButtonRole::Destructive)
        .on_open(move || records.revoke_key(&id))
    };
    // What the agent may reach says on a line of its own that wraps: it is the whole of
    // what the person let in, and a narrow window must not cut it short.
    let mut row = faced(
        ListRow::new(format!("console-grant-{index}"), grant.agent.clone()),
        &agent,
    )
    .secondary(access(grant))
    .secondary_lines(2)
    .meta(status)
    .trailing(Box::new(revoke));
    if grant.browser_pages {
        row = row.tags(&[(
            tr("console-grant-browser-pages").as_str(),
            ChipStyle::Neutral,
        )]);
    }
    if !grant.sessions.is_empty() {
        row = row.active();
    }
    Box::new(row)
}
