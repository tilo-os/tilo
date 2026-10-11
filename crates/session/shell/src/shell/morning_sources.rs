//! Nonblocking, currently admitted provider previews for explicitly chosen resources.
//! A catalogue read grants nothing and never invokes a provider refresh.
use super::certified_action_session::ActionSessionSource;
use super::collab_broker::AppActionBroker;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tilo_actions::remote::{ActionHostError, CataloguePage, CatalogueQuery, Pending, PendingPoll};
use tilo_desktop::activity::morning::{MAX_SOURCES, MorningPayload, MorningSource};

const CHECK_INTERVAL: Duration = Duration::from_secs(5);
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

type Reply = Pending<Result<CataloguePage, ActionHostError>>;
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fence {
    identity: Option<u64>,
    policy: u64,
    inventory: u64,
    directory: u64,
}
enum Phase {
    Providers,
    Payload,
}
struct Entry {
    fence: Fence,
    checked: Instant,
    pending: Option<(Phase, Reply)>,
    payload: Option<MorningPayload>,
}
pub(crate) struct NativeMorningSources {
    broker: AppActionBroker,
    sessions: ActionSessionSource,
    // Only the active room and at most the pane's eight choices are retained.
    entries: RefCell<HashMap<(String, String), Entry>>,
    commands: RefCell<HashMap<(String, String), CommandState>>,
}
impl NativeMorningSources {
    pub(crate) fn new(broker: AppActionBroker) -> Self {
        Self {
            sessions: ActionSessionSource::new(broker.clone()),
            broker,
            entries: RefCell::default(),
            commands: RefCell::default(),
        }
    }
    pub(crate) fn payload(&self, room: &str, source: &MorningSource) -> Option<MorningPayload> {
        let fence = Fence {
            identity: self.sessions.identity_revision(room),
            policy: tilo_identity_host::policy_change::epoch(),
            inventory: self.broker.surfaces_generation(),
            directory: self.broker.action_directory_root_page(None, 0).generation,
        };
        let mut entries = self.entries.borrow_mut();
        if fence.identity.is_none() {
            entries.clear();
            return None;
        }
        entries.retain(|(entry_room, _), entry| {
            entry_room == room && entry.checked.elapsed() < Duration::from_secs(60)
        });
        let key = (room.to_owned(), source.key());
        if entries
            .get(&key)
            .is_some_and(|entry| entry.fence != fence || entry.checked.elapsed() >= CHECK_INTERVAL)
        {
            entries.remove(&key);
        }
        if !entries.contains_key(&key) {
            if entries.len() >= MAX_SOURCES {
                return None;
            }
            let reply = self.sessions.open(room, None).ok()?.query(CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            });
            entries.insert(
                key.clone(),
                Entry {
                    fence,
                    checked: Instant::now(),
                    pending: Some((
                        Phase::Providers,
                        super::search::invocation_wake::watch(reply),
                    )),
                    payload: None,
                },
            );
        }
        let entry = entries.get_mut(&key)?;
        self.advance(entry, room, source);
        entry.payload.clone()
    }
    fn advance(&self, entry: &mut Entry, room: &str, source: &MorningSource) {
        let Some((phase, pending)) = entry.pending.as_mut() else {
            return;
        };
        let result = pending.poll_ready();
        if matches!(result, PendingPoll::Waiting) && entry.checked.elapsed() < REPLY_TIMEOUT {
            return;
        }
        let PendingPoll::Ready(Ok(page)) = result else {
            entry.pending = None;
            entry.payload = None;
            return;
        };
        match phase {
            Phase::Providers => {
                // Multiple live records cannot be guessed from a history-only locator.
                let mut providers = page.providers.iter().filter(|provider| {
                    provider.room == room && provider.app_id == source.app && provider.browsable
                });
                let Some(provider) = providers.next() else {
                    entry.pending = None;
                    return;
                };
                if providers.next().is_some() {
                    entry.pending = None;
                    return;
                }
                let Ok(session) = self.sessions.open(room, None) else {
                    entry.pending = None;
                    return;
                };
                let reply = session.query(CatalogueQuery {
                    provider: Some(provider.provider.clone()),
                    needle: source.resource.clone(),
                    max_results: 32,
                    limits: Some(tilo_actions::remote::CatalogueLimits {
                        max_bytes: Some(32_768),
                        max_work: Some(128),
                        max_depth: Some(8),
                    }),
                    ..CatalogueQuery::default()
                });
                entry.pending =
                    Some((Phase::Payload, super::search::invocation_wake::watch(reply)));
            }
            Phase::Payload => {
                entry.payload = payload_from_page(&page, source);
                entry.pending = None;
            }
        }
    }
}
fn payload_from_page(page: &CataloguePage, source: &MorningSource) -> Option<MorningPayload> {
    if page.rows.len() > 32 {
        return None;
    }
    page.rows
        .iter()
        .filter(|row| row.app_id == source.app && row.resource.id == source.resource)
        .find_map(|row| {
            let presentation = row.resource.presentation.as_ref()?;
            (presentation.is_valid()
                && presentation
                    .preview
                    .as_ref()
                    .is_some_and(|preview| !preview.trim().is_empty()))
            .then(|| MorningPayload {
                label: row.resource.label.clone(),
                presentation: presentation.clone(),
                refreshable: page.rows.iter().any(|row| refresh_row(row, source)),
            })
        })
}

struct CommandState {
    pending:
        Option<Pending<Result<(u64, tilo_actions::remote::InvocationReceipt), ActionHostError>>>,
    started: Instant,
    message: String,
}
fn refresh_row(row: &tilo_actions::remote::CatalogueRow, source: &MorningSource) -> bool {
    row.app_id == source.app
        && row.resource.id == source.resource
        && row.action.as_ref().is_some_and(|action| {
            action.address.resource == source.resource
                && action.address.action == "refresh"
                && action.kind == tilo_actions::OperationKind::Effect
                && action.contract.permission == tilo_actions::PermissionRequirement::Read
                && action.contract.confirmation == tilo_actions::ConfirmationPolicy::None
                && action.params.iter().all(|parameter| !parameter.required)
        })
}
impl NativeMorningSources {
    pub(crate) fn submit(
        &self,
        room: &str,
        source: &MorningSource,
        command: tilo_desktop::activity::morning::MorningCommand,
    ) -> Result<(), String> {
        use tilo_actions::remote::{InvokeArgument, InvokeRequest, application_action_target};
        use tilo_desktop::activity::morning::MorningCommand;
        if command != MorningCommand::Open
            && !self
                .payload(room, source)
                .is_some_and(|payload| payload.refreshable)
        {
            return Err(super::i18n::t("activity-morning-command-unavailable"));
        }
        let mut commands = self.commands.borrow_mut();
        commands.retain(|(entry_room, _), _| entry_room == room);
        let key = (room.to_owned(), source.key());
        if commands
            .get(&key)
            .is_some_and(|state| state.pending.is_some())
        {
            return Err(super::i18n::t("activity-morning-command-pending"));
        }
        if !commands.contains_key(&key) && commands.len() >= MAX_SOURCES {
            return Err(super::i18n::t("activity-morning-command-unavailable"));
        }
        let (target, argument) = match command {
            MorningCommand::NextSource => {
                return Err(super::i18n::t("activity-morning-command-unavailable"));
            }
            MorningCommand::Open => (
                application_action_target(room, &source.app, &source.resource, "open"),
                InvokeArgument::None,
            ),
            MorningCommand::Refresh => (
                application_action_target(room, &source.app, &source.resource, "refresh"),
                InvokeArgument::None,
            ),
            MorningCommand::DraftRefresh => (
                application_action_target(
                    room,
                    tilo_automation::SHORTCUTS_APP_ID,
                    "shortcuts.workbench",
                    "draft-shortcut",
                ),
                InvokeArgument::Json(serde_json::Map::from_iter([(
                    "shortcut".into(),
                    serde_json::to_value(refresh_draft(source))
                        .map_err(|error| error.to_string())?,
                )])),
            ),
        };
        let reply = self.sessions.open(room, None)?.invoke_job(InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target,
            argument,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
        });
        commands.insert(
            key,
            CommandState {
                pending: Some(super::search::invocation_wake::watch(reply)),
                started: Instant::now(),
                message: super::i18n::t("activity-morning-command-pending"),
            },
        );
        Ok(())
    }
    pub(crate) fn command_status(&self, room: &str, source: &MorningSource) -> Option<String> {
        let mut commands = self.commands.borrow_mut();
        let state = commands.get_mut(&(room.to_owned(), source.key()))?;
        if let Some(reply) = state.pending.as_mut() {
            let result = reply.poll_ready();
            match result {
                PendingPoll::Waiting if state.started.elapsed() < Duration::from_secs(40) => {}
                PendingPoll::Ready(Ok((_, receipt)))
                    if matches!(
                        receipt.status,
                        tilo_actions::remote::ReceiptStatus::Accepted
                            | tilo_actions::remote::ReceiptStatus::Completed { .. }
                    ) =>
                {
                    state.pending = None;
                    state.message = super::i18n::t("activity-morning-command-accepted");
                    self.entries
                        .borrow_mut()
                        .remove(&(room.to_owned(), source.key()));
                }
                _ => {
                    state.pending = None;
                    state.message = super::i18n::t("activity-morning-command-refused");
                }
            }
        }
        Some(state.message.clone())
    }
}
/// Only an inert editor program. Saving later uses the existing exact-action
/// grant review and owner-checked shortcut library; this builder grants nothing.
fn refresh_draft(source: &MorningSource) -> tilo_automation::Shortcut {
    tilo_automation::Shortcut {
        id: format!("morning-{}", uuid::Uuid::new_v4()),
        name: super::i18n::t("activity-morning-refresh"),
        inputs: Vec::new(),
        steps: vec![tilo_automation::Step::new(
            "refresh",
            tilo_automation::StepKind::Invoke {
                app_id: source.app.clone(),
                resource: tilo_automation::Selector::Fixed(source.resource.clone()),
                action: "refresh".into(),
                args: std::collections::BTreeMap::default(),
            },
        )],
        output: None,
        grant: Vec::new(),
        triggers: Vec::new(),
        consents: Vec::new(),
        saved_by: None,
    }
}

#[cfg(test)]
#[path = "morning_sources_tests.rs"]
mod tests;
