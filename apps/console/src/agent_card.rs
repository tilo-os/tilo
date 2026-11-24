//! An agent's card: one client ("Claude Code") as a member, beside the person's work.
//!
//! A turn a trigger started is not a client session: its state is the
//! `#answer` invocation record in `tilo.invocations` (status, steps, its output
//! with cost and summary), and stopping it is that record's `cancel`. A view of
//! an agent's work reads those records rather than keeping a second list.

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_actions::remote::mcp_server;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::tokens::TextRole;
use ui::widget::{ButtonRole, ListRow, Section, SectionHeader};

use crate::agents::{self, Seen};
use crate::effects::{AgentCommand, ConsoleEffect};
use crate::{Console, Nav, Reads, Resources, ago, faced, page, tr, trf};

pub(crate) const AGENT_KIND: &str = "console.agent";

pub(crate) use console_model::Agent;

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
    let agent = || [("agent", serde_json::Value::from(client.to_owned()))];
    let bindings = ui::tilo_resource::RowBindings::new()
        .bind(ConsoleEffect::VIEW_AGENT, agent())
        .bind(ConsoleEffect::STOP_AGENT, agent())
        .bind(ConsoleEffect::UNDO_AGENT_SESSION, agent())
        .bind(ConsoleEffect::TAKE_OVER_AGENT, agent())
        .bind(ConsoleEffect::REVOKE_AGENT, agent());
    LiveRow::typed_with::<Agent>(agent_resource_id(client), client.to_owned(), &bindings)
        .view(card_view())
}

/// Every client `names` (sessions' agents, grants) name, each once, in order.
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

/// The sessions of `client`, newest first.
fn sessions_of<'a>(seen: &'a Seen, client: &str) -> Vec<&'a wire::AgentSession> {
    let mut sessions: Vec<&wire::AgentSession> = seen
        .history
        .sessions
        .iter()
        .filter(|session| crate::client::of(&session.agent.name).name() == client)
        .collect();
    sessions.sort_by_key(|session| std::cmp::Reverse(session.last_ms));
    sessions
}

/// The grant `client` came in by, when the MCP server lists one.
fn grant_of<'a>(seen: &'a Seen, client: &str) -> Option<&'a mcp_server::Grant> {
    seen.server
        .grants
        .iter()
        .find(|grant| crate::client::of(&grant.agent).name() == client)
}

/// Carry out `command` on the agent of `client`, as its card's buttons do.
pub(crate) fn act(records: &Resources, seen: &Seen, client: &str, command: AgentCommand) -> bool {
    let newest = sessions_of(seen, client).into_iter().next();
    match command {
        AgentCommand::Stop => newest.is_some_and(|session| agents::stop(records, seen, session)),
        AgentCommand::UndoSession => {
            newest.is_some_and(|session| agents::undo_session(records, session))
        }
        AgentCommand::TakeOver => newest.is_some_and(|session| {
            agents::take_over(records, seen, session);
            true
        }),
        AgentCommand::Revoke => grant_of(seen, client).is_some_and(|grant| {
            records.revoke_key(&grant.id);
            true
        }),
    }
}

/// The card of `client`: once the sessions and the MCP server have answered, the command asked from
/// outside it (`nav.command`) is carried out, once.
pub(crate) fn view(cx: &ViewCx<Console>, nav: &Nav, reads: &Reads, client: &str) -> Element {
    let server_state = reads.server.answer().state();
    let (nav, client) = (nav.clone(), client.to_owned());
    Box::new(cx.arriving(&reads.sessions.answer(), move |cx, sessions| {
        let (nav, sessions, client) = (nav.clone(), sessions.clone(), client.clone());
        cx.switch(&server_state, move |cx, server| {
            let seen = Seen {
                history: sessions.clone(),
                server: agents::ready(server),
            };
            if let Some(command) = nav.command.get()
                && !matches!(server, Arrival::Pending { .. })
            {
                nav.command.set(None);
                act(&Resources::of(cx), &seen, &client, command);
            }
            card(cx, &nav, &seen, &client)
        })
    }))
}

fn card(cx: &ViewCx<Console>, nav: &Nav, seen: &Seen, client: &str) -> Element {
    let records = Resources::of(cx);
    let sessions = sessions_of(seen, client);
    let newest = sessions.first().copied();
    let working = newest.filter(|session| seen.working(session));
    let face = wire::Author {
        name: client.to_owned(),
        relation: wire::Relation::Software,
        ..wire::Author::default()
    };
    let now = match (working, newest) {
        (Some(session), _) => session
            .now
            .as_ref()
            .map_or_else(|| session.instruction.clone(), agents::now_line),
        (None, Some(session)) => trf(
            "console-agent-last-worked",
            &[("when", ago(session.last_ms).into())],
        ),
        (None, None) => tr("console-agent-no-sessions"),
    };
    let mut head =
        faced(ListRow::new("console-agent-head", client.to_owned()), &face).subtitle(now);
    if working.is_some() {
        head = head.active();
    }
    let mut parts: Vec<Element> = vec![Box::new(head)];
    if let Some(session) = newest {
        parts.push(agents::commands(&records, seen, session));
        if let Some(blocked) = &session.undo_blocked {
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
            let (records, id) = (records.clone(), grant.id.clone());
            Button::new("console-agent-revoke", "console-grant-revoke")
                .role(ButtonRole::Destructive)
                .on_open(move || records.revoke_key(&id))
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
    let rows: Vec<Element> = sessions
        .iter()
        .map(|session| agents::session_row(nav, seen, session, false))
        .collect();
    if rows.is_empty() {
        parts.push(Box::new(
            EmptyState::new(tr("console-agent-no-sessions"))
                .with_icon(Icon::Bot)
                .with_id("console-agent-empty"),
        ));
    } else {
        parts.push(Box::new(
            Section::new(rows).with_header(SectionHeader::group(tr("console-agent-sessions"))),
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
        assert_eq!(agent_of("console.session:Claude Code"), None);
    }

    /// The desktop asks these of an agent by name (its face's card, a notice): each is one
    /// of the card's own actions.
    #[test]
    fn the_cards_actions_are_the_names_the_desktop_asks_for() {
        assert_eq!(ConsoleEffect::VIEW_AGENT.id, "view-agent");
        assert_eq!(ConsoleEffect::STOP_AGENT.id, "stop-agent");
        assert_eq!(ConsoleEffect::UNDO_AGENT_SESSION.id, "undo-agent-session");
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
