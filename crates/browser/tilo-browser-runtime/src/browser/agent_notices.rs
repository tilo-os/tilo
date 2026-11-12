//! What a page tells its person about agents in their workspace that they did not cause: an
//! agent finishing ("Claude Code finished: 4 changes in Storefront", counted from its run as the
//! workspace's history holds it), and what they chose on such a notice (its card in Console).

use super::Desktop;

/// Console, where an agent's card is.
const CONSOLE_APP: &str = "so.tilo.console";
/// The action key of a notice's choice that opens the card of the agent of the client after it.
const AGENT_CARD_ACTION: &str = "agent-card:";
/// How long a finished agent's notice waits for the history to say what its run did.
const FINISHED_READ_MS: u64 = 3_000;

/// The notice that `agent` finished: what its run changed and where, when `summary` says
/// ("Claude Code finished: 4 changes in Storefront"), else that it finished; its last line under
/// it, and its card one choice away.
fn finished_notice(
    agent: &tilo_desktop::here::AgentFinished,
    summary: Option<&tilo_desktop::here::RunSummary>,
) -> tilo_desktop_surface::notifications::Notification {
    use tilo_desktop_surface::i18n::{t, t_args};
    let name = || agent.label.clone().into();
    let title = match summary {
        Some(tilo_desktop::here::RunSummary {
            changes,
            workspace: Some(workspace),
        }) => t_args(
            "workspace-notice-agent-finished-in",
            &[
                ("agent", name()),
                ("count", (*changes).into()),
                ("workspace", workspace.clone().into()),
            ],
        ),
        Some(tilo_desktop::here::RunSummary {
            changes,
            workspace: None,
        }) => t_args(
            "workspace-notice-agent-finished-changes",
            &[("agent", name()), ("count", (*changes).into())],
        ),
        None => t_args("workspace-notice-agent-finished", &[("agent", name())]),
    };
    let client = ui::presence::agent_client(&agent.label);
    let mut notice = tilo_desktop_surface::notifications::Notification::new(
        title,
        agent.last.clone().unwrap_or_default(),
    )
    .with_from(agent.label.clone(), agent.publisher.clone())
    .with_app_id(CONSOLE_APP)
    // It is the agent's news, said in its name wherever notices are grouped by who sent them; the
    // banner, which leads with the agent's face, shows no source line repeating it.
    .with_app_name(agent.label.clone());
    notice.actions = vec![(
        format!("{AGENT_CARD_ACTION}{client}"),
        t("workspace-notice-agent-card"),
    )];
    notice
}

impl Desktop {
    /// An agent finishing is news this person did not cause: a notice says
    /// what came of it ("Claude Code finished: 4 changes in Storefront"),
    /// counted from its run as the workspace's history holds it, and
    /// choosing it opens the agent's card ([`Self::follow_notice_choices`]).
    /// The history is asked first; the notice waits for its answer, at most
    /// [`FINISHED_READ_MS`], and says only that the agent finished without
    /// one.
    pub(super) fn announce_agents_finished(&self) {
        use tilo_actions::remote::workspace_activity as wire;
        use tilo_actions::remote::{ActionHost as _, InvokeRequest};
        let now = crate::shell::now_ms();
        let finished = self.here.borrow_mut().take_agents_finished(now);
        for agent in finished {
            let invocation = tilo_actions::InvocationId::new();
            let request = InvokeRequest {
                invocation,
                target: wire::target(wire::RUNS),
                argument: wire::argument(&wire::RunsQuery {
                    scope: wire::Scope::ThisWorkspace,
                    limit: 0,
                }),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
            };
            drop(self.session.invoke(&self.caller(), request));
            self.finishing.borrow_mut().push((agent, invocation, now));
        }
        let waiting = std::mem::take(&mut *self.finishing.borrow_mut());
        for (agent, invocation, asked_ms) in waiting {
            let answered = self
                .session
                .observe(&self.caller(), &invocation)
                .and_then(|state| match state.update {
                    tilo_actions::InvocationUpdate::Item { value, .. } => Some(value),
                    tilo_actions::InvocationUpdate::Succeeded { output, .. } => output.value,
                    _ => None,
                })
                .and_then(|value| serde_json::from_value::<wire::Runs>(value).ok());
            let timed_out = now.saturating_sub(asked_ms) >= FINISHED_READ_MS;
            if answered.is_none() && !timed_out {
                self.finishing
                    .borrow_mut()
                    .push((agent, invocation, asked_ms));
                continue;
            }
            drop(self.session.cancel(&self.caller(), &invocation));
            // The history names the workspace of a run it read across workspaces; read in this
            // one, the run worked here.
            let here_name = self.handle.read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.name.trim().to_owned())
                    .filter(|name| !name.is_empty())
            });
            let summary =
                answered
                    .as_ref()
                    .and_then(|runs| agent.summary(runs))
                    .map(|mut summary| {
                        if summary.workspace.is_none() {
                            summary.workspace = here_name;
                        }
                        summary
                    });
            self.post(finished_notice(&agent, summary.as_ref()));
        }
    }

    /// When a notice of an agent finishing is next due, in unix ms: an agent gone past the
    /// reconnect's grace, or a run's history read past its wait. The page wakes for it: it draws
    /// only when something changes, and an agent that stays gone changes nothing.
    pub(super) fn agent_notices_due_ms(&self) -> Option<u64> {
        let reads = self
            .finishing
            .borrow()
            .iter()
            .map(|(_, _, asked_ms)| asked_ms.saturating_add(FINISHED_READ_MS))
            .min();
        let left = self.here.borrow().next_agent_finished_ms();
        reads.into_iter().chain(left).min()
    }

    /// Carry out what the person chose on a notice the page posted itself:
    /// an agent's notice opens its card in Console.
    pub(super) fn follow_notice_choices(&self) {
        use tilo_desktop_surface::NotificationOutcome;
        for outcome in self.persistence.notifications.take_outcomes() {
            let NotificationOutcome::Activated {
                action: Some(action),
                ..
            } = outcome
            else {
                continue;
            };
            if let Some(client) = action.strip_prefix(AGENT_CARD_ACTION) {
                self.shared
                    .ask_app_action(tilo_desktop::launchpad::LaunchpadAppAction::ask_agent(
                        client,
                        tilo_desktop::profile_card::AgentAsk::Card,
                    ));
            }
        }
    }
}
