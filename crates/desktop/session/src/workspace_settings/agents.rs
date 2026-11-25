//! The Agents tab: who works in the workspace, held in the workspace's list of its agents.
//! An agent is a workspace resource of its own type ([`tilo_app::tilo_workspace_agent::KIND`]),
//! kept by the workspace ([`tilo_app::tilo_workspace_agent::OWNER`]) and followed here through the
//! device's one read, as every app of the workspace reads it ([`FollowedResource`]). Making,
//! naming, dressing and retiring an agent are the workspace's own actions (`workspace/agents`),
//! which only the workspace's service carries out ([`AgentsChange`]), decided there by the
//! workspace's grants — so the tab, the CLI, agents and the person's other devices change the one
//! list the same way, and the tab neither decides nor writes anything of its own.
//! What an agent may do, app by app, is still the workspace's grants: the tab reads them beside
//! the list. An agent a person admitted to the workspace before the list named it is added to it
//! when a member's tab follows it ([`Agents::settle`]), under the id its grants name, so the
//! roster, the name a person reads and the search find the agents that worked here.
//! The agents are the workspace's, not any one member's: their model and what they may spend a
//! day are the list's, and whoever the workspace's grants let change it chooses them.

use std::cell::RefCell;

use tilo_action_policy::{ActionKind, MemberId};
use tilo_app::tilo_workspace_agent::{
    KIND, OWNER, WorkspaceAgents as Record, workspace_agents as actions,
};
use tilo_desktop_model::{Layout, Workspace};
use tilo_resource::Id;
use tilo_session_client::daemon_documents::DaemonShell;
use tilo_session_core::desktop::authority::WorkspaceAuthority as _;
use tilo_session_core::desktop::i18n::{t, t_args};
use tilo_session_core::desktop::people::PeopleNames;
use tilo_session_core::desktop::workspace_settings::{self as domain, agent_access};
use tilo_sheets::workspace_settings as sheet;

use crate::followed_resource::FollowedResource;

/// One change of a workspace's record of its agents, as the tab and the session ask the
/// workspace for it: the actions its governance tree publishes under `workspace/agents`, the only
/// way the record changes.
pub(crate) enum AgentsChange {
    /// Make an agent.
    Create(actions::CreateAgent),
    /// Change one: any of the record's actions but making and retiring.
    Update(tilo_resource::Invoke<Record>),
    /// Retire one, by its id.
    Retire(String),
}

impl AgentsChange {
    /// The workspace's action this change is, and its arguments.
    pub(crate) fn call(self) -> (&'static str, serde_json::Map<String, serde_json::Value>) {
        use tilo_actions::remote::{
            WORKSPACE_AGENT_CREATE, WORKSPACE_AGENT_DELETE, WORKSPACE_AGENT_UPDATE,
        };
        match self {
            Self::Create(create) => (
                WORKSPACE_AGENT_CREATE,
                Record::CREATE_AGENT.with(create).arguments,
            ),
            Self::Update(update) => (
                WORKSPACE_AGENT_UPDATE,
                serde_json::Map::from_iter([
                    ("action".to_owned(), update.id.into()),
                    (
                        "arguments".to_owned(),
                        serde_json::Value::Object(update.arguments),
                    ),
                ]),
            ),
            Self::Retire(agent) => (
                WORKSPACE_AGENT_DELETE,
                serde_json::Map::from_iter([("agent".to_owned(), agent.into())]),
            ),
        }
    }
}

impl AgentsChange {
    /// This change as the workspace's governance action it is, asked as the person.
    fn request(self, workspace: &str) -> tilo_actions::remote::InvokeRequest {
        let (operation, arguments) = self.call();
        tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget::workspace_governance(
                workspace, operation,
            ),
            argument: tilo_actions::remote::InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }
}

/// The agent's id a change answered, for one made: the action's own value, which the daemon's
/// dispatch hands its callers.
fn made(answer: &serde_json::Value) -> String {
    answer
        .get("agent")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// What the tab made of one of the sheet's asks.
pub(crate) enum Answered {
    /// The tab is shown again: the review of the agent named, or the roster.
    Show(Option<String>),
    /// Nothing to show anew: the list's change shows when it lands.
    Done,
    /// The change was refused, in the person's words.
    Refused(String),
}

/// The workspace's agents as the Agents tab holds them.
pub(crate) struct Agents {
    /// The device's daemon, where the workspace's list of its agents is read and changed.
    daemon: DaemonShell,
    /// Where this computer's own workspace service keeps what it keeps of its workspaces: the
    /// models it keeps for each workspace's agents.
    service_root: Option<std::path::PathBuf>,
    /// Wakes the session's loop when the list moved.
    wake: tilo_service::Wake,
    /// The list, once the tab showed it: the workspace, the person and the grants it was followed
    /// at.
    open: RefCell<Option<Open>>,
    /// What the tab last drew: the list's revision. The tab is shown again at each change.
    shown: RefCell<Option<Seen>>,
    /// The list whose admitted agents this device was last brought in step with: its workspace
    /// and revision.
    settled: RefCell<Option<(String, u64)>>,
}

/// The list as the tab follows it.
struct Open {
    workspace: String,
    person: String,
    /// The grants it was followed at: they decide what the tab offers, so a workspace whose
    /// grants changed follows it again.
    policy: tilo_action_policy::WorkspacePolicy,
    record: FollowedResource<Record>,
}

/// What the tab drew at one frame.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Seen {
    revision: u64,
}

/// What the person may change on the tab: the workspace's agents, and what each may do here.
#[derive(Clone, Copy)]
struct May {
    manage: bool,
}

/// What one agent's review reads: the list that keeps it, the workspace it works in, the agents
/// the tab lists, and this device's apps and Settings.
#[derive(Clone, Copy)]
struct Review<'a> {
    person: &'a str,
    record: &'a FollowedResource<Record>,
    workspace: &'a Workspace,
    apps: &'a dyn domain::AppDirectory,
    names: &'a PeopleNames,
    config: &'a agent_provider_config::ProviderConfig,
}

impl Agents {
    /// The workspace's agents, read and changed at the device's `daemon`, waking the session's
    /// loop through `wake` when the list moved. `service_root` is where this computer's own
    /// workspace service keeps the models its agents answer on.
    pub(crate) fn new(
        daemon: DaemonShell,
        service_root: Option<std::path::PathBuf>,
        wake: tilo_service::Wake,
    ) -> Self {
        Self {
            daemon,
            service_root,
            wake,
            open: RefCell::default(),
            shown: RefCell::default(),
            settled: RefCell::default(),
        }
    }

    /// The models this computer's workspace service keeps for `workspace`: the ones its agents
    /// here answer on, and where this computer's Agent models pane links its choice.
    fn models(&self, workspace: &Workspace) -> agent_provider_config::ProviderConfig {
        self.service_root
            .as_deref()
            .map_or_else(Default::default, |root| {
                tilo_workspace_service::workspaces::this_computer::workspace_models_at(
                    root,
                    &workspace.id().to_string(),
                )
            })
    }

    /// The Agents tab as it stands for `person` : the agents that work in the workspace from Chat's
    /// record, and, for `review`, what one of them may do.
    pub(crate) fn pane(
        &self,
        tree: &Layout,
        person: &str,
        (names, apps): (&PeopleNames, &dyn domain::AppDirectory),
        review: Option<&str>,
    ) -> sheet::Agents {
        let Some(workspace) = tree.active_workspace() else {
            return self.not_available();
        };
        let Some(record) = self.record(Some(workspace), person) else {
            return self.not_available();
        };
        *self.shown.borrow_mut() = Some(Seen {
            revision: record.revision(),
        });
        let may = May {
            manage: {
                use tilo_session_core::desktop::authority::WorkspaceAuthority as _;
                workspace.may_administer(person)
            },
        };
        let config = self.models(workspace);
        Self::pane_of(
            workspace,
            &record,
            person,
            (names, apps, &config),
            may,
            review,
        )
    }

    /// The tab with nothing to show here: no workspace in front, or the agents could not be opened
    /// on this device.
    fn not_available(&self) -> sheet::Agents {
        *self.shown.borrow_mut() = Some(Seen { revision: 0 });
        sheet::Agents {
            intro: sheet::Intro {
                title: t_args("workspace-settings-agents-count", &[("count", 0.into())]),
                help: t("workspace-settings-agents-help"),
                authority: t("workspace-settings-view-only"),
            },
            add: t("workspace-settings-add-agent"),
            create: t("workspace-settings-create-agent"),
            cancel: t("workspace-settings-cancel"),
            name: t("workspace-settings-agent-name"),
            model: t("workspace-settings-agent-model"),
            model_default: t("workspace-settings-agent-model-default-choice"),
            manage: false,
            header: t("workspace-settings-agents-header"),
            empty: (
                t("workspace-settings-agents-empty-title"),
                t("workspace-settings-agents-unavailable"),
            ),
            ..sheet::Agents::default()
        }
    }

    /// The tab as `workspace` stands for the person `record` is opened as.
    fn pane_of(
        workspace: &Workspace,
        record: &FollowedResource<Record>,
        person: &str,
        (names, apps, config): (
            &PeopleNames,
            &dyn domain::AppDirectory,
            &agent_provider_config::ProviderConfig,
        ),
        may: May,
        review: Option<&str>,
    ) -> sheet::Agents {
        let state = record.get();
        let manage = may.manage;
        let agents: Vec<sheet::Agent> = state
            .made_by_people()
            .map(|agent| sheet::Agent {
                key: agent.id.0.clone(),
                name: agent.name.clone(),
                status: added_by(names, workspace, record, &agent.acts_for),
                spend: spend_line(agent),
                model: model_line(config, &agent.id.0, agent.model.as_deref()),
            })
            .collect();
        let count = agents.len();
        let review = review.and_then(|agent| {
            Self::reviewed(
                &Review {
                    person,
                    record,
                    workspace,
                    apps,
                    names,
                    config,
                },
                agent,
                may,
            )
        });
        sheet::Agents {
            intro: sheet::Intro {
                title: t_args(
                    "workspace-settings-agents-count",
                    &[("count", count.into())],
                ),
                help: t("workspace-settings-agents-help"),
                authority: if manage {
                    t("workspace-settings-you-manage")
                } else {
                    t("workspace-settings-view-only")
                },
            },
            add: t("workspace-settings-add-agent"),
            create: t("workspace-settings-create-agent"),
            cancel: t("workspace-settings-cancel"),
            name: t("workspace-settings-agent-name"),
            model: t("workspace-settings-agent-model"),
            model_default: t("workspace-settings-agent-model-default-choice"),
            models: agent_access::agent_models(config, "")
                .map(|models| {
                    models
                        .offered
                        .into_iter()
                        .map(|option| sheet::ModelChoice {
                            key: option.key(),
                            label: option.label,
                            in_use: false,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            manage,
            header: t("workspace-settings-agents-header"),
            agents,
            review_label: t("workspace-settings-review-access"),
            retire_label: t("workspace-settings-retire-agent"),
            empty: (
                t("workspace-settings-agents-empty-title"),
                t("workspace-settings-agents-empty-detail"),
            ),
            footnote: (!manage).then(|| t("workspace-settings-owner-only")),
            review,
        }
    }

    /// Each app's row in a review: its kinds, each with the answer it has now, as the segmented
    /// choice shows them.
    fn access_apps(rows: &[agent_access::AgentAccessRow]) -> Vec<sheet::AccessApp> {
        let answer_index = |answer: tilo_action_policy::Answer| {
            agent_access::ANSWERS
                .iter()
                .position(|candidate| *candidate == answer)
                .unwrap_or_default()
        };
        rows.iter()
            .map(|row| sheet::AccessApp {
                icon: Self::access_icon(&row.app),
                scope: row.scope.key(),
                label: row.label.clone(),
                customised: row.customised(),
                kinds: row
                    .cells
                    .iter()
                    .map(|cell| sheet::KindLine {
                        kind: agent_access::kind_key(cell.kind).to_owned(),
                        label: agent_access::kind_label(cell.kind),
                        answer: answer_index(cell.answer),
                        choices: row.choices(cell),
                        // Only the person's own account rows say what reaching outside means.
                        caption: (cell.kind == ActionKind::Outside)
                            .then(|| row.effects.clone())
                            .flatten(),
                    })
                    .collect(),
            })
            .collect()
    }

    /// The mark a row wears: an app's own glyph, and the people's mark where the row governs
    /// the workspace's members and settings.
    fn access_icon(app: &str) -> ui::icons::Icon {
        if app == tilo_action_policy::agent_answers::WORKSPACE_APP {
            ui::icons::Icon::Users
        } else {
            ui::icons::app_glyph(app, None)
        }
    }

    /// One agent's review: what it may do in each app, the model it plans with and the actions it
    /// may always take. `None` once the record no longer names it.
    fn reviewed(review: &Review<'_>, agent: &str, may: May) -> Option<sheet::AgentReview> {
        let Review {
            person,
            record,
            workspace,
            apps,
            names,
            config,
        } = *review;
        let state = record.get();
        let kept = state.get(&Id::from_raw(agent.to_owned()))?;
        // An entry made by no person's root key, which no device of ours writes, is not shown.
        MemberId::parsed_person(&kept.acts_for)?;
        let name = kept.name.clone();
        let status = added_by(names, workspace, record, &kept.acts_for);
        // What the turn admits it to, before the answers the workspace keeps for it: what the
        // approval of the person it acts for gives it.
        let pair = (agent, kept.acts_for.as_str());
        let admission = tilo_action_policy::roles::certified_agent(
            &workspace.policy.workspace,
            pair,
            kept.acts_for.as_str(),
        );
        let rows = agent_access::agent_access_rows(workspace, pair, &admission, apps);
        let exceptions = agent_access::agent_exceptions(workspace, pair, apps);
        // What it plans with, what it may spend and what it may do are the record's: whoever the
        // workspace's grants let change the record changes them, and nobody else.
        let offered = agent_access::agent_models(config, agent);
        // The person it acts for answers for it; so does one who may grant here.
        let editable = person == kept.acts_for.as_str() || workspace.may_administer(person);
        let limit = limit_of(kept, record.may("set-agent-spend-limit"));
        let can_choose = record.may("set-agent-model");
        let chosen = kept.model.as_deref();
        let models = sheet::Models {
            title: t("workspace-settings-agent-model"),
            summary: chosen.map_or_else(
                || model_summary(offered.as_ref()),
                |model| model_label(offered.as_ref(), model),
            ),
            choices: model_choices(offered.as_ref(), chosen),
            can_choose,
            note: (!can_choose).then(|| t("workspace-settings-agent-model-view-only")),
        };
        Some(sheet::AgentReview {
            agent: agent.to_owned(),
            name,
            status,
            tag: None,
            back: t("workspace-settings-agent-access-back"),
            note: t("workspace-settings-agent-access-note"),
            editable,
            can_retire: may.manage || record.may("retire-agent"),
            retire: t("workspace-settings-retire-agent"),
            answers: [
                t("workspace-settings-answer-allow"),
                t("workspace-settings-answer-ask"),
                t("workspace-settings-answer-deny"),
            ],
            reset: t("workspace-settings-agent-access-reset"),
            custom: t("workspace-settings-agent-access-set-here"),
            apps: Self::access_apps(&rows),
            models: Some(models),
            limit: Some(limit),
            use_model: t("workspace-settings-agent-model-use"),
            in_use: t("workspace-settings-agent-model-in-use"),
            exceptions_title: t_args(
                "workspace-settings-agent-access-always",
                &[("count", exceptions.len().into())],
            ),
            exceptions: exceptions
                .into_iter()
                .map(|exception| sheet::Exception {
                    scope: exception.scope.key(),
                    kind: agent_access::kind_key(exception.kind).to_owned(),
                    label: exception.label,
                })
                .collect(),
            stop: t("workspace-settings-agent-access-stop"),
        })
    }

    /// Whether the tab would show something else now: the record moved, or it could not be opened
    /// yet and is asked for again. `false` before the tab first opened it.
    #[cfg(target_os = "linux")]
    pub(crate) fn moved(&self, tree: &Layout, person: &str) -> bool {
        let Some(workspace) = tree.active_workspace() else {
            return false;
        };
        let held = self.open.borrow();
        let Some(open) = held
            .as_ref()
            .filter(|open| open.workspace == workspace.id().to_string())
            .filter(|open| open.person == *person)
        else {
            return false;
        };
        let record = &open.record;
        self.shown.borrow().as_ref()
            != Some(&Seen {
                revision: record.revision(),
            })
    }

    /// The list of `workspace` as `person`, followed here when it is not yet, and followed again
    /// when the workspace's grants changed, so what the tab offers follows them.
    fn record(
        &self,
        workspace: Option<&Workspace>,
        person: &str,
    ) -> Option<FollowedResource<Record>> {
        let workspace = workspace?;
        let workspace_id = workspace.id().to_string();
        let held = self.open.borrow();
        let current = held.as_ref().filter(|open| {
            open.workspace == workspace_id
                && open.person == *person
                && open.policy == workspace.policy
        });
        if let Some(open) = current {
            return Some(open.record.clone());
        }
        drop(held);
        let record = FollowedResource::follow(
            &self.daemon,
            (
                &workspace_id,
                OWNER,
                &collab_types::workspace_document_id(KIND),
            ),
            person,
            Some(workspace.policy.clone()),
            std::sync::Arc::clone(&self.wake),
        );
        *self.open.borrow_mut() = Some(Open {
            workspace: workspace_id,
            person: person.to_owned(),
            policy: workspace.policy.clone(),
            record: record.clone(),
        });
        Some(record)
    }

    /// Take up what the record owes this device while the tab is up: one
    /// record for every agent the workspace admitted before records were.
    ///
    /// Nothing happens until the tab opened the record here: no other tab
    /// reads it.
    #[cfg(target_os = "linux")]
    pub(crate) fn settle(&self, tree: &Layout, person: &str) {
        let Some(workspace) = tree.active_workspace() else {
            return;
        };
        let workspace_id = workspace.id().to_string();
        let held = self.open.borrow();
        let record = held
            .as_ref()
            .filter(|open| open.workspace == workspace_id)
            .filter(|open| open.person == *person)
            .filter(|open| open.policy == workspace.policy)
            .map(|open| open.record.clone());
        drop(held);
        let Some(record) = record else {
            return;
        };
        if self.settled.borrow().as_ref() == Some(&(workspace_id.clone(), record.revision())) {
            return;
        }
        let config = agent_provider_config::ProviderConfig::load();
        self.record_admitted(workspace, person, &record, &config);
        *self.settled.borrow_mut() = Some((workspace_id, record.revision()));
    }

    /// Make the record name every agent admitted to `workspace`: one made for each admission that
    /// has none, under the id its grants carry, so the roster, the names people read and the
    /// search find the agents that worked here before records were. Any member the workspace's
    /// grants let change the record makes it; a device whose member may not leaves it to one who
    /// may.
    /// An admission names no name of its own — its id is no name a person reads — so each is
    /// numbered, in id order, around the names the workspace already uses.
    /// A model chosen for the agent on this device before the record held one goes into the
    /// record with it.
    #[cfg(target_os = "linux")]
    fn record_admitted(
        &self,
        workspace: &Workspace,
        person: &str,
        record: &FollowedResource<Record>,
        config: &agent_provider_config::ProviderConfig,
    ) {
        let state = record.get();
        let mut taken: Vec<String> = state.active().map(|agent| agent.name.clone()).collect();
        let mut number: u64 = 0;
        for (member, _) in workspace.policy.agent_members_at(tilo_clock::unix_now()) {
            // The record this person asks for is of their own agents: one admitted for another
            // person is theirs to record.
            let Some((agent, _)) = member
                .as_agent()
                .filter(|(_, of)| of.as_str().eq_ignore_ascii_case(person))
            else {
                continue;
            };
            if state.get(&Id::from_raw(agent.to_owned())).is_some() {
                continue;
            }
            // A person admitted it: a service's membership is no agent here.
            let prefix = format!("agent-member:{agent}:");
            let by_a_person = workspace
                .policy
                .grants
                .iter()
                .filter(|grant| grant.id.as_str().starts_with(&prefix))
                .any(|grant| grant.granted_by.person_id().is_some());
            if !by_a_person {
                continue;
            }
            let name = loop {
                number = number.saturating_add(1);
                let name = t_args(
                    "workspace-settings-agent-name-number",
                    &[("number", number.into())],
                );
                if !taken.contains(&name) {
                    break name;
                }
            };
            let Some(name) = tilo_resource::NonBlank::parse(&name) else {
                continue;
            };
            taken.push(name.as_str().to_owned());
            let model = agent_access::agent_models(config, agent)
                .and_then(|models| models.chosen.map(|chosen| chosen.key()));
            // Asked of the workspace's service off the loop: the next pass reads what it wrote.
            // Until it does, or where the workspace's grants refuse the person, the tab still
            // lists the agent from its grants.
            let request = AgentsChange::Create(actions::CreateAgent {
                avatar: None,
                name,
                id: Some(agent.to_owned()),
                model,
                mcp_client: None,
            })
            .request(&workspace.id().to_string());
            let daemon = self.daemon.clone();
            self.daemon.runtime().spawn(async move {
                let message = tilo_actions::remote::RemoteActionMessage::Invoke(request);
                if let Err(refusal) = daemon.dispatch(None, &message).await {
                    tracing::warn!(%refusal, "an admitted agent is not on the list yet");
                }
            });
        }
    }

    /// Carry out an ask of the Agents tab for `person` in `workspace` (the one in front): making,
    /// dressing and retiring an agent are the record's own actions. `None` for an ask
    /// that is no agent's.
    pub(crate) fn answer(
        &self,
        workspace: Option<&Workspace>,
        person: &str,
        effect: &sheet::WorkspaceSettingsEffect,
    ) -> Option<Answered> {
        use sheet::WorkspaceSettingsEffect as Ask;
        let shown = |done: Result<(), String>, review: Option<&str>| match done {
            Ok(()) => Answered::Show(review.map(str::to_owned)),
            Err(reason) => Answered::Refused(reason),
        };
        Some(match effect {
            Ask::AddAgent { name, model } => {
                // The record is the admission: the agent works here because Chat's record names
                // it.
                match self.create(workspace, person, (name.as_str(), None), model) {
                    Ok(_) => Answered::Done,
                    Err(reason) => Answered::Refused(reason),
                }
            }
            // The review was of an agent the workspace no longer lists.
            Ask::RemoveAgent { agent } => shown(self.retire(workspace, agent), None),
            Ask::SetAgentModel { agent, model } => {
                shown(self.set_model(workspace, agent, model), Some(agent))
            }
            Ask::SetAgentSpendLimit { agent, dollars } => {
                // What its field held, in dollars, or no limit at all where it held nothing.
                let limit_micros = if dollars.is_empty() {
                    None
                } else {
                    let Some(micros) = ui::format::parse_money(dollars) else {
                        return Some(Answered::Refused(t(
                            "workspace-settings-agent-limit-not-amount",
                        )));
                    };
                    Some(micros)
                };
                shown(self.set_limit(workspace, agent, limit_micros), Some(agent))
            }
            _ => return None,
        })
    }

    /// Make an agent called `name` as the workspace lets the person, and answer its id and the
    /// name the record keeps for it, so the caller can list it and admit it.
    ///
    /// # Errors
    /// The workspace's agents could not be opened here, no name was typed, or Chat's grants refuse
    /// the change.
    pub(crate) fn create(
        &self,
        workspace: Option<&Workspace>,
        person: &str,
        (name, mcp_client): (&str, Option<&str>),
        model: &str,
    ) -> Result<(String, String), String> {
        let name = tilo_resource::NonBlank::parse(name.trim())
            .ok_or_else(|| t("workspace-settings-agent-name-needed"))?;
        // The id the record frees for the name, as this device reads it; the service picks one
        // itself where this device holds no record yet.
        let id = self
            .record(workspace, person)
            .map(|record| record.get().free_id(name.as_str()));
        let id = self.ask(
            workspace,
            AgentsChange::Create(actions::CreateAgent {
                avatar: None,
                name: name.clone(),
                id,
                model: (!model.is_empty()).then(|| model.to_owned()),
                mcp_client: mcp_client.map(str::to_owned),
            }),
        )?;
        Ok((id, name.as_str().to_owned()))
    }

    /// Set the picture `agent` wears wherever an author is shown, as the picture a profile keeps:
    /// a bounded `data:` URL of a small raster image, or none for its initials.
    ///
    /// # Errors
    /// The workspace's grants refuse the change.
    #[cfg(test)]
    pub(crate) fn set_avatar(
        &self,
        workspace: Option<&Workspace>,
        agent: &str,
        avatar: Option<&str>,
    ) -> Result<(), String> {
        self.ask(
            workspace,
            AgentsChange::Update(Record::SET_AGENT_AVATAR.with(actions::SetAgentAvatar {
                agent: Id::from_raw(agent.to_owned()),
                avatar: avatar.map(str::to_owned),
            })),
        )
        .map(drop)
    }

    /// The active agent `person` keeps in `workspace` for the MCP client `client`, with the name
    /// the record keeps for it.
    #[cfg(target_os = "linux")]
    pub(crate) fn of_client(
        &self,
        workspace: Option<&Workspace>,
        person: &str,
        client: &str,
    ) -> Option<(String, String)> {
        let record = self.record(workspace, person)?;
        let state = record.get();
        state
            .active()
            .find(|agent| agent.acts_for == *person && agent.mcp_client.as_deref() == Some(client))
            .map(|agent| (agent.id.0.clone(), agent.name.clone()))
    }

    /// The member the agent `agent` of the list of `workspace` is, as `person` reads the list:
    /// its id and the person it acts for. `None` for an agent the list does not keep.
    pub(crate) fn member(
        &self,
        workspace: Option<&Workspace>,
        person: &str,
        agent: &str,
    ) -> Option<MemberId> {
        let record = self.record(workspace, person)?;
        let state = record.get();
        state
            .get(&Id::from_raw(agent.to_owned()))
            .map(tilo_app::tilo_workspace_agent::Agent::member)
    }

    /// Retire the agent `agent`: it takes no new work, and what it did stays
    /// readable.
    ///
    /// # Errors
    /// The workspace's agents could not be opened here, or Chat's grants
    /// refuse the change.
    pub(crate) fn retire(&self, workspace: Option<&Workspace>, agent: &str) -> Result<(), String> {
        self.ask(workspace, AgentsChange::Retire(agent.to_owned()))
            .map(drop)
    }

    /// Choose the model the agent `agent` plans with.
    ///
    /// # Errors
    /// The workspace's agents could not be opened here, or Chat's grants refuse
    /// the change.
    pub(crate) fn set_model(
        &self,
        workspace: Option<&Workspace>,
        agent: &str,
        model: &str,
    ) -> Result<(), String> {
        self.ask(
            workspace,
            AgentsChange::Update(Record::SET_AGENT_MODEL.with(actions::SetAgentModel {
                agent: Id::from_raw(agent.to_owned()),
                model: (!model.is_empty()).then(|| model.to_owned()),
            })),
        )
        .map(drop)
    }

    /// Set what the agent `agent` may spend a day: `None` spends unmetered.
    ///
    /// # Errors
    /// The workspace's agents could not be opened here, or Chat's grants refuse
    /// the change.
    pub(crate) fn set_limit(
        &self,
        workspace: Option<&Workspace>,
        agent: &str,
        limit_micros: Option<u64>,
    ) -> Result<(), String> {
        self.ask(
            workspace,
            AgentsChange::Update(
                Record::SET_AGENT_SPEND_LIMIT.with(actions::SetAgentSpendLimit {
                    agent: Id::from_raw(agent.to_owned()),
                    limit_micros,
                }),
            ),
        )
        .map(drop)
    }

    /// Ask the workspace for `change` and wait for its answer, then take in what it wrote: the
    /// agent's id for one made.
    ///
    /// # Errors
    /// No workspace is in front, the workspace's grants or the record refuse the change, or the
    /// service did not answer in time.
    fn ask(&self, workspace: Option<&Workspace>, change: AgentsChange) -> Result<String, String> {
        let workspace = workspace.ok_or_else(|| t("workspace-settings-agents-unavailable"))?;
        let message = tilo_actions::remote::RemoteActionMessage::Invoke(
            change.request(&workspace.id().to_string()),
        );
        let answer = self.daemon.dispatch_blocking(None, &message)?;
        // What it wrote shows at once, not once the follow next reads it.
        if let Some(open) = self.open.borrow().as_ref() {
            open.record.refresh();
        }
        Ok(made(&answer))
    }
}

/// Today, as a day's counter keeps it: this device's clock.
fn today() -> String {
    ui::clock::today().to_string()
}

/// What an agent may spend a day, as its review shows it: the workspace's limit, what it spent of
/// it, and — where the person here may change it, `can_choose` — the way to set it.
fn limit_of(agent: &tilo_app::tilo_workspace_agent::Agent, can_choose: bool) -> sheet::Limit {
    sheet::Limit {
        title: t("workspace-settings-agent-limit"),
        summary: limit_summary(agent.spend_limit_micros),
        today: t_args(
            "workspace-settings-agent-limit-today",
            &[("spent", ui::format::money(agent.spent_on(&today())).into())],
        ),
        note: if can_choose {
            t("workspace-settings-agent-limit-note")
        } else {
            t("workspace-settings-agent-limit-view-only")
        },
        placeholder: t("workspace-settings-agent-limit-placeholder"),
        set: t("workspace-settings-agent-limit-set"),
        can_choose,
    }
}

/// What `agent` spends of its day, as its row reads it: the money it spent today beside what it
/// may, or only the money where no limit is set.
fn spend_line(agent: &tilo_app::tilo_workspace_agent::Agent) -> String {
    let spent = ui::format::money(agent.spent_on(&today()));
    match agent.spend_limit_micros {
        Some(limit) => t_args(
            "workspace-settings-agent-spend-of",
            &[
                ("spent", spent.into()),
                ("limit", ui::format::money(limit).into()),
            ],
        ),
        None if agent.spent_on(&today()) > 0 => {
            t_args("workspace-settings-agent-spend", &[("spent", spent.into())])
        }
        None => String::new(),
    }
}

/// What an agent may spend a day, as its review reads it.
fn limit_summary(limit_micros: Option<u64>) -> String {
    limit_micros.map_or_else(
        || t("workspace-settings-agent-limit-none"),
        |limit| {
            t_args(
                "workspace-settings-agent-limit-amount",
                &[("limit", ui::format::money(limit).into())],
            )
        },
    )
}

/// Who added an agent, as its row reads it: `made_by`, the member its record names.
fn added_by(
    names: &PeopleNames,
    workspace: &Workspace,
    record: &FollowedResource<Record>,
    made_by: &str,
) -> String {
    if made_by == record.me().id() {
        return t("workspace-settings-agent-added-by-you");
    }
    let Some(member) = MemberId::parsed_person(made_by) else {
        return made_by.to_owned();
    };
    t_args(
        "workspace-settings-agent-added-by",
        &[(
            "name",
            domain::member_row_name(names, workspace, record.me().id(), &member).into(),
        )],
    )
}

/// The model `agent` plans with, as its row reads it: the one chosen for it, else the
/// workspace's default, or that none is chosen yet.
fn model_line(
    config: &agent_provider_config::ProviderConfig,
    agent: &str,
    chosen: Option<&str>,
) -> String {
    let offered = agent_access::agent_models(config, agent);
    let label = chosen.map_or_else(
        || {
            offered
                .as_ref()
                .and_then(|offered| offered.default.clone())
                .unwrap_or_else(|| t("workspace-settings-agent-model-none"))
        },
        |model| model_label(offered.as_ref(), model),
    );
    t_args(
        "workspace-settings-agent-model-line",
        &[("model", label.into())],
    )
}

/// The models a person may choose for an agent, the one in use marked: the
/// default first, then every model this device's Settings offer, and — where
/// the choice was made on a device that offers more — the model it runs with,
/// so the review always shows it.
fn model_choices(
    models: Option<&agent_access::AgentModels>,
    in_use: Option<&str>,
) -> Vec<sheet::ModelChoice> {
    let mut choices: Vec<sheet::ModelChoice> = vec![sheet::ModelChoice {
        key: String::new(),
        label: t("workspace-settings-agent-model-none"),
        in_use: in_use.is_none(),
    }];
    for option in models.into_iter().flat_map(|models| &models.offered) {
        choices.push(sheet::ModelChoice {
            key: option.key(),
            label: option.label.clone(),
            in_use: in_use == Some(option.key().as_str()),
        });
    }
    if let Some(model) = in_use
        && !choices.iter().any(|choice| choice.key == model)
    {
        choices.push(sheet::ModelChoice {
            key: model.to_owned(),
            label: model.to_owned(),
            in_use: true,
        });
    }
    choices
}

/// What an agent plans with by default, as the review's models row reads it.
fn model_summary(models: Option<&agent_access::AgentModels>) -> String {
    let model = models
        .and_then(|models| models.default.clone())
        .unwrap_or_else(|| t("workspace-settings-agent-model-none"));
    t_args(
        "workspace-settings-agent-model-default",
        &[("model", model.into())],
    )
}

/// The model `key` names as the person reads it, as this device's Settings offer it; the key itself
/// where they do not.
fn model_label(models: Option<&agent_access::AgentModels>, key: &str) -> String {
    models
        .and_then(|models| {
            models
                .offered
                .iter()
                .find(|option| option.key() == key)
                .map(|option| option.label.clone())
        })
        .unwrap_or_else(|| key.to_owned())
}
