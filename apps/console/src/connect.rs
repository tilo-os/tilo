//! Connect: the one line that connects an MCP client to the person's
//! workspaces, and every agent the door let in, with Revoke.

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::agent_door as door;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::widget::{ButtonRole, ListRow, Property, PropertyList, Section};

use crate::{Console, Doors, Reads, ago, faced, page, tr, trf};

const fn client_label(client: door::Client) -> &'static str {
    match client {
        door::Client::ClaudeCode => "console-client-claude-code",
        door::Client::Codex => "console-client-codex",
        door::Client::Other => "console-client-other",
    }
}

pub(crate) fn view(cx: &ViewCx<Console>, reads: &Reads) -> Element {
    let doors = Doors::of(cx);
    let read = &reads.door;
    Box::new(cx.arriving(&read.answer(), move |_, door| {
        let commands = door::Client::ALL
            .into_iter()
            .map(|client| {
                Property::new(tr(client_label(client)), client.command(&door.endpoint))
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
        let agents: Vec<Element> = if door.grants.is_empty() {
            vec![Box::new(
                EmptyState::new(tr("console-grants-empty"))
                    .with_message(tr("console-grants-empty-message"))
                    .with_icon(Icon::Link)
                    .with_id("console-grants-empty"),
            )]
        } else {
            door.grants
                .iter()
                .enumerate()
                .map(|(index, grant)| grant_row(&doors, index, grant))
                .collect()
        };
        parts.push(Box::new(Section::titled(
            tr("console-grants-heading"),
            agents,
        )));
        page(parts)
    }))
}

/// Where `reach` lets the agent work, and what else its key may do, in
/// words: the reach first, then whether the key operates web pages in
/// Browser without asking, as the door's own grants page says it.
pub(crate) fn access(grant: &door::Grant) -> String {
    let reach = match &grant.reach {
        door::Reach::Account => tr("console-reach-account"),
        door::Reach::Workspace { name, .. } => trf(
            "console-reach-workspace",
            &[("workspace", name.clone().into())],
        ),
    };
    if grant.browser_pages {
        trf("console-reach-browser-pages", &[("reach", reach.into())])
    } else {
        reach
    }
}

fn grant_row(doors: &Doors, index: usize, grant: &door::Grant) -> Element {
    let agent = wire::Author {
        name: grant.agent.clone(),
        relation: wire::Relation::Software,
        kind: ui::tilo_actions::ActorKind::Agent,
        ..wire::Author::default()
    };
    let status = if grant.runs.is_empty() {
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
            &[("count", grant.runs.len().into())],
        )
    };
    let revoke = {
        let (doors, id) = (doors.clone(), grant.id.clone());
        Button::new(
            &format!("console-grant-{index}-revoke"),
            "console-grant-revoke",
        )
        .role(ButtonRole::Destructive)
        .on_view(move || {
            doors.door(door::REVOKE, &door::GrantRef { grant: id.clone() });
        })
    };
    // What the agent may reach says on a line of its own that wraps: it is
    // the whole of what the person let in, and a narrow window must not cut
    // it short.
    let mut row = faced(
        ListRow::new(format!("console-grant-{index}"), grant.agent.clone()),
        &agent,
    )
    .secondary(access(grant))
    .secondary_lines(2)
    .meta(status)
    .trailing(Box::new(revoke));
    if !grant.runs.is_empty() {
        row = row.active();
    }
    Box::new(row)
}
