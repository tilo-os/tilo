//! An agent's card: one client ("Claude Code") as a collaborator, beside the person's work.

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::agent_door as door;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::tokens::TextRole;
use ui::widget::{ButtonRole, ListRow, Section, SectionHeader};

use crate::agents::{self, Seen};
use crate::effects::{AgentCommand, ConsoleEffect};
use crate::{Console, Doors, Nav, Reads, ago, faced, page, tr, trf};

pub(crate) const AGENT_KIND: &str = "console.agent";

/// An agent as the type its rows are items of: what a published agent says it is, so search
/// names it by its type rather than by a tag the card writes.
pub(crate) struct Agent;

impl ui::tilo_resource::words::RowType for Agent {
    const KIND: &'static str = AGENT_KIND;
    const LABEL: &'static str = "Agent";
}

/// The address of the agent of `client`.
pub(crate) fn agent_resource_id(client: &str) -> String {
    format!("{AGENT_KIND}:{client}")
}

/// The client whose card `resource` addresses, when it is an agent's.
pub(crate) fn agent_of(resource: &str) -> Option<&str> {
    resource
        .strip_prefix(AGENT_KIND)?
        .strip_prefix(':')
        .filter(|client| !client.is_empty())
}

fn card_view() -> ui::tilo_actions::ResourceViewSpec {
    ui::tilo_actions::ResourceViewSpec::new("card", "Card", ConsoleEffect::VIEW_AGENT.id)
        .with_primary(true)
}

/// The agent of `client` as the resource its address names: its card, and the card's
/// commands.
pub(crate) fn live_agent(client: &str) -> LiveRow {
    let agent = || client.to_owned();
    LiveRow::typed::<Agent>(agent_resource_id(client), client.to_owned())
        .effect(&ConsoleEffect::ViewAgent { agent: agent() })
        .effect(&ConsoleEffect::StopAgent { agent: agent() })
        .effect(&ConsoleEffect::UndoAgentRun { agent: agent() })
        .effect(&ConsoleEffect::TakeOverAgent { agent: agent() })
        .effect(&ConsoleEffect::RevokeAgent { agent: agent() })
        .view(card_view())
}

/// Every client `names` (runs' agents, grants) name, each once, in order.
pub(crate) fn clients<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut clients: Vec<String> = Vec::new();
    for name in names {
        let client = crate::client::of(name).name().to_owned();
        if !client.is_empty() && !clients.contains(&client) {
            clients.push(client);
        }
    }
    clients
}

/// The runs of `client`, newest first.
fn runs_of<'a>(seen: &'a Seen, client: &str) -> Vec<&'a wire::AgentRun> {
    let mut runs: Vec<&wire::AgentRun> = seen
        .runs
        .runs
        .iter()
        .filter(|run| crate::client::of(&run.agent.name).name() == client)
        .collect();
    runs.sort_by_key(|run| std::cmp::Reverse(run.last_ms));
    runs
}

/// The grant `client` came in by, when the door lists one.
fn grant_of<'a>(seen: &'a Seen, client: &str) -> Option<&'a door::Grant> {
    seen.door
        .grants
        .iter()
        .find(|grant| crate::client::of(&grant.agent).name() == client)
}

/// Carry out `command` on the agent of `client`, as its card's buttons do.
pub(crate) fn act(doors: &Doors, seen: &Seen, client: &str, command: AgentCommand) -> bool {
    let newest = runs_of(seen, client).into_iter().next();
    match command {
        AgentCommand::Stop => newest.is_some_and(|run| agents::stop(doors, seen, run)),
        AgentCommand::UndoRun => newest.is_some_and(|run| agents::undo_run(doors, run)),
        AgentCommand::TakeOver => newest.is_some_and(|run| {
            agents::take_over(doors, seen, run);
            true
        }),
        AgentCommand::Revoke => grant_of(seen, client).is_some_and(|grant| {
            doors.door(
                door::REVOKE,
                &door::GrantRef {
                    grant: grant.id.clone(),
                },
            );
            true
        }),
    }
}

/// The card of `client`: once the runs and the door have answered, the command asked from
/// outside it (`nav.command`) is carried out, once.
pub(crate) fn view(cx: &ViewCx<Console>, nav: &Nav, reads: &Reads, client: &str) -> Element {
    let door_state = reads.door.answer().state();
    let (nav, client) = (nav.clone(), client.to_owned());
    Box::new(cx.arriving(&reads.runs.answer(), move |cx, runs| {
        let (nav, runs, client) = (nav.clone(), runs.clone(), client.clone());
        cx.switch(&door_state, move |cx, door| {
            let seen = Seen {
                runs: runs.clone(),
                door: agents::ready(door),
            };
            if let Some(command) = nav.command.get()
                && !matches!(door, Arrival::Pending { .. })
            {
                nav.command.set(None);
                act(&Doors::of(cx), &seen, &client, command);
            }
            card(cx, &nav, &seen, &client)
        })
    }))
}

fn card(cx: &ViewCx<Console>, nav: &Nav, seen: &Seen, client: &str) -> Element {
    let doors = Doors::of(cx);
    let runs = runs_of(seen, client);
    let newest = runs.first().copied();
    let working = newest.filter(|run| seen.working(run));
    let face = wire::Author {
        name: client.to_owned(),
        relation: wire::Relation::Software,
        kind: ui::tilo_actions::ActorKind::Agent,
        ..wire::Author::default()
    };
    let now = match (working, newest) {
        (Some(run), _) => run
            .now
            .as_ref()
            .map_or_else(|| run.instruction.clone(), agents::now_line),
        (None, Some(run)) => trf(
            "console-agent-last-worked",
            &[("when", ago(run.last_ms).into())],
        ),
        (None, None) => tr("console-agent-no-runs"),
    };
    let mut head =
        faced(ListRow::new("console-agent-head", client.to_owned()), &face).subtitle(now);
    if working.is_some() {
        head = head.active();
    }
    let mut parts: Vec<Element> = vec![Box::new(head)];
    if let Some(run) = newest {
        parts.push(agents::commands(&doors, seen, run));
        if let Some(blocked) = &run.undo_blocked {
            parts.push(Box::new(
                Text::new(blocked)
                    .with_role(TextRole::Caption)
                    .dimmed()
                    .with_id("console-agent-undo-blocked"),
            ));
        }
    }
    if let Some(grant) = grant_of(seen, client) {
        let revoke = {
            let (doors, id) = (doors.clone(), grant.id.clone());
            Button::new("console-agent-revoke", "console-grant-revoke")
                .role(ButtonRole::Destructive)
                .on_view(move || {
                    doors.door(door::REVOKE, &door::GrantRef { grant: id.clone() });
                })
        };
        parts.push(Box::new(Section::titled(
            tr("console-agent-access"),
            vec![Box::new(
                ListRow::new("console-agent-access-row", crate::connect::access(grant))
                    .icon(Icon::Link)
                    .secondary_lines(2)
                    .trailing(Box::new(revoke)),
            )],
        )));
    }
    let rows: Vec<Element> = runs
        .iter()
        .map(|run| agents::run_row(nav, seen, run, false))
        .collect();
    if rows.is_empty() {
        parts.push(Box::new(
            EmptyState::new(tr("console-agent-no-runs"))
                .with_icon(Icon::Bot)
                .with_id("console-agent-empty"),
        ));
    } else {
        parts.push(Box::new(
            Section::new(rows).with_header(SectionHeader::group(tr("console-agent-runs"))),
        ));
    }
    page(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agents_address_names_its_client() {
        assert_eq!(
            agent_resource_id("Claude Code"),
            "console.agent:Claude Code"
        );
        assert_eq!(agent_of("console.agent:Claude Code"), Some("Claude Code"));
        assert_eq!(agent_of("console.agent:"), None);
        assert_eq!(agent_of("console.run:Claude Code"), None);
    }

    /// The desktop asks these of an agent by name (its face's card, a notice): each is one
    /// of the card's own actions.
    #[test]
    fn the_cards_actions_are_the_names_the_desktop_asks_for() {
        assert_eq!(ConsoleEffect::VIEW_AGENT.id, "view-agent");
        assert_eq!(ConsoleEffect::STOP_AGENT.id, "stop-agent");
        assert_eq!(ConsoleEffect::UNDO_AGENT_RUN.id, "undo-agent-run");
        assert_eq!(ConsoleEffect::TAKE_OVER_AGENT.id, "take-over-agent");
        assert_eq!(ConsoleEffect::REVOKE_AGENT.id, "revoke-agent");
    }

    #[test]
    fn each_client_is_listed_once_from_its_runs_and_its_grant() {
        assert_eq!(
            clients(["Claude Code #2", "Claude Code #1", "Codex"]),
            vec!["Claude Code".to_owned(), "Codex".to_owned()]
        );
    }
}
