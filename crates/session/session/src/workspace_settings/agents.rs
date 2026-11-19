//! The Agents tab: who works in the workspace, held in Chat's list of the workspace's agents.
//! An agent is a workspace record of its own kind ([`tilo_app::tilo_workspace_agent::KIND`]), kept
//! by Chat ([`tilo_app::tilo_workspace_agent::OWNER`]) and opened here the way every app of the
//! workspace opens it ([`WorkspaceResource`]): in the store this device's apps keep, as the person
//! sitting here. Naming, bounding and retiring an agent are the record's own actions, decided by
//! the workspace's grants on Chat's node — the decision the app windows and the action broker take
//! — so the tab, the apps and the person's other devices hold the one record, and the tab decides
//! nothing of its own.
//! What an agent may do, app by app, is still the workspace's grants: the tab reads them beside the
//! record. An agent a person admitted to the workspace before records were is made one of the
//! record's when that person's tab opens it ([`Agents::settle`]), under the id its grants name and
//! kept by them, so the roster, the name a person reads and the search find the agents that worked
//! here. Nobody makes the record of an agent somebody else admitted: whoever makes one keeps it.
//! The model an agent plans with is the keeper's to choose, alone: their devices run it and their
//! credentials pay. This device applies the keeper's choice to its own Settings when the tab is
//! open ([`agent_access::choose_agent_model`]), which is where the agent runner reads it from; the
//! record is the one truth the tab shows.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_action_policy::agent_answers::AnswerScope;
use tilo_action_policy::{ActionKind, Answer, MemberId};
use tilo_app::framework::WorkspaceResource;
use tilo_app::tilo_workspace_agent::{
    Agent, Bound, OWNER, WorkspaceAgents as Record, workspace_agents as actions,
};
use tilo_desktop::people::PeopleNames;
use tilo_desktop::workspace_settings::{self as domain, agent_access};
use tilo_desktop_model::{Layout, UserId, Workspace};
use tilo_desktop_surface::i18n::{t, t_args};
use tilo_resource::Id;
use tilo_sheets::workspace_settings as sheet;
use ui::permissions::LivePermissions;
use ui::resource::workspace::WorkspaceResources;

/// How workspace settings' history names where a change came from.
const SETTINGS_APP: (&str, &str) = ("tilo-session", "Workspace settings");
/// Whether an agent made here runs on the workspace's host: a page runs no agent, so one made in a
/// browser works on the host around the clock; one made on a computer runs there, on its keeper's
/// devices and models.
const RUNS_ON_HOST: bool = cfg!(target_arch = "wasm32");

/// Where the tab opens the workspace's records, and what it knows of the person there: a
/// computer's copy of its workspaces, or a page's.
pub trait Host {
    /// The records of `workspace` as `person` opens them, and the grants that decide their
    /// actions where this host knows them.
    fn open(
        &self,
        workspace: &Workspace,
        person: &UserId,
    ) -> (WorkspaceResources, Option<LivePermissions>);

    /// Whether `person` looks after `workspace`'s agents: makes, bounds and retires them.
    fn manages(&self, workspace: &Workspace, person: &UserId) -> bool;
}

/// A computer's copy of its workspaces: their records, decided by the grants the workspace's
/// governance keeps here.
#[cfg(target_os = "linux")]
impl Host for crate::engine_records::EngineRecords {
    fn open(
        &self,
        workspace: &Workspace,
        person: &UserId,
    ) -> (WorkspaceResources, Option<LivePermissions>) {
        use ui::permissions::PermissionContext;
        let context = PermissionContext::from_access(tilo_action_policy::MemberAccess::person(
            workspace.policy.clone(),
            person.clone(),
        ))
        .with_policy_time_sampler(std::sync::Arc::new(|| Ok(tilo_clock::unix_now())));
        let records = self.in_workspace(
            &workspace.id().to_string(),
            ui::permissions::MemberId::person(person.clone()),
        );
        (records, Some(LivePermissions::fixed(context)))
    }

    fn manages(&self, workspace: &Workspace, person: &UserId) -> bool {
        use tilo_desktop::authority::WorkspaceAuthority as _;
        workspace.may_administer(person)
    }
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
    /// The workspace's records as the workspace holds them: the copy Chat and every app of the
    /// workspace read.
    host: Rc<dyn Host>,
    /// Wakes the session's loop when another replica's change arrives.
    wake: ui::WakeScope,
    /// The record, once the tab showed it: the workspace, the person and the grants it was opened
    /// for.
    open: RefCell<Option<Open>>,
    /// What the tab last drew: the list's revision. The tab is shown again at each change.
    shown: RefCell<Option<Seen>>,
    /// The record whose admitted agents and keeper's choices this device was last brought in step
    /// with: its workspace and revision.
    #[cfg(target_os = "linux")]
    settled: RefCell<Option<(String, u64)>>,
}

/// The record as the tab opened it.
struct Open {
    workspace: String,
    person: UserId,
    /// The grants it was opened with: they decide its actions, so a workspace whose grants changed
    /// opens the record again.
    policy: tilo_action_policy::WorkspacePolicy,
    /// `None` where the record could not be opened here.
    record: Option<WorkspaceResource<Record>>,
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
    record: &'a WorkspaceResource<Record>,
    workspace: &'a Workspace,
    apps: &'a dyn domain::AppDirectory,
    names: &'a PeopleNames,
    config: &'a agent_provider_config::ProviderConfig,
}

impl Agents {
    /// The workspace's agents in the records `host` opens, waking the session's loop through
    /// `wake` when a change lands.
    pub(crate) fn new(host: Rc<dyn Host>, wake: tilo_service::Wake) -> Self {
        let mut scope = ui::WakeScope::new();
        if let Err(error) = scope.bind(move |_| wake()) {
            tracing::warn!(%error, "the workspace's agents are not woken");
        }
        Self {
            host,
            wake: scope,
            open: RefCell::default(),
            shown: RefCell::default(),
            #[cfg(target_os = "linux")]
            settled: RefCell::default(),
        }
    }

    /// The Agents tab as it stands for `person` : the agents that work in the workspace from Chat's
    /// record, the services beside them — its server's moderation agent where
    /// `moderation` says its server offers one — and, for `review`, what one of them may do.
    pub(crate) fn pane(
        &self,
        tree: &Layout,
        person: &UserId,
        (names, apps): (&PeopleNames, &dyn domain::AppDirectory),
        moderation: bool,
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
            manage: self.host.manages(workspace, person),
        };
        let mut pane = Self::pane_of(workspace, &record, (names, apps), may, review);
        pane.moderation = moderation.then(|| moderation_row(workspace, may));
        pane
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
        record: &WorkspaceResource<Record>,
        (names, apps): (&PeopleNames, &dyn domain::AppDirectory),
        may: May,
        review: Option<&str>,
    ) -> sheet::Agents {
        let state = record.get();
        let manage = may.manage;
        let config = agent_provider_config::ProviderConfig::load();
        let agents: Vec<sheet::Agent> = state
            .made_by_people()
            .map(|agent| sheet::Agent {
                key: agent.id.0.clone(),
                name: agent.name.clone(),
                status: keeper_line(names, workspace, record, &agent.keeper),
                spend: spend_line(agent),
                model: if agent.on_host {
                    t("workspace-settings-agent-on-host")
                } else {
                    model_line(&config, &agent.id.0, agent.model.as_deref())
                },
            })
            .collect();
        let count = agents.len();
        let review = review.and_then(|agent| {
            Self::reviewed(
                &Review {
                    record,
                    workspace,
                    apps,
                    names,
                    config: &config,
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
            // An agent made here runs on the workspace's server, on its model: nothing to choose.
            models: agent_access::agent_models(&config, "")
                .filter(|_| !RUNS_ON_HOST)
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
            offers_bots: true,
            bots_header: t("workspace-settings-bots-header"),
            bots_help: t("workspace-settings-bots-help"),
            add_bot: t("workspace-settings-add-bot"),
            bot_tag: t("workspace-settings-bot-tag"),
            remove_bot: t("workspace-settings-remove-bot"),
            bots: workspace
                .bots
                .iter()
                .map(|bot| sheet::Bot {
                    name: bot.name.clone(),
                    entry: bot.requested.then(|| bot.entry.clone()).flatten(),
                    detail: domain::bot_detail(bot),
                })
                .collect(),
            // Shown where the workspace's server offers it ([`Self::pane`]).
            moderation: None,
            bots_empty: t("workspace-settings-bots-empty"),
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
        if app == tilo_action_policy::agent_answers::WORKSPACE_GOVERNANCE_APP {
            ui::icons::Icon::Users
        } else {
            ui::icons::app_glyph(app, None)
        }
    }

    /// One agent's review: what it may do in each app, the model it plans with and the actions it
    /// may always take. `None` once the record no longer names it.
    fn reviewed(review: &Review<'_>, agent: &str, may: May) -> Option<sheet::AgentReview> {
        let Review {
            record,
            workspace,
            apps,
            names,
            config,
        } = *review;
        let state = record.get();
        let kept = state.get(&Id::from_raw(agent.to_owned()))?;
        // An entry whose keeper is no person's root key, which no device of ours writes, is
        // not shown.
        MemberId::parsed_person(&kept.keeper)?;
        let name = kept.name.clone();
        let status = keeper_line(names, workspace, record, &kept.keeper);
        let tag = (kept.keeper == record.me().id()).then(|| t("workspace-settings-agent-yours"));
        // What the turn admits it to, before the answers the person set for its apps: the cap's
        // grants, narrowed by the bound its record keeps.
        let mut admission = tilo_action_policy::Acting::handed_agent(agent, kept.keeper.as_str())
            .handed_preset(&workspace.policy.workspace);
        admission.extend(tilo_action_policy::roles::agent_bound(
            agent,
            &kept.keeper,
            &kept.bound_rules().unwrap_or_default(),
        ));
        let rows = agent_access::agent_access_rows(workspace, agent, &admission, apps);
        let exceptions =
            agent_access::agent_exceptions(&kept.bound_rules().unwrap_or_default(), apps);
        // What an agent plans with is its keeper's to choose, alone, and the record holds the
        // choice.
        let offered = agent_access::agent_models(config, agent);
        let keeps = kept.keeper == record.me().id();
        // What it may do here is the bound its list entry keeps: whoever looks after the workspace's agents,
        // and its keeper, set it.
        let editable = may.manage || keeps;
        let keeper = domain::member_row_name(
            names,
            workspace,
            record.me().id(),
            &MemberId::person(&kept.keeper),
        );
        let limit = limit_of(kept, keeps, &keeper);
        let note = if keeps {
            t("workspace-settings-agent-model-keeper-you")
        } else {
            t_args(
                "workspace-settings-agent-model-keeper",
                &[("name", keeper.into())],
            )
        };
        let chosen = kept.model.as_deref();
        let models = sheet::Models {
            title: t("workspace-settings-agent-model"),
            summary: chosen.map_or_else(
                || model_summary(offered.as_ref()),
                |model| model_label(offered.as_ref(), model),
            ),
            choices: model_choices(offered.as_ref(), chosen),
            can_choose: keeps,
            note: Some(note),
        };
        Some(sheet::AgentReview {
            agent: agent.to_owned(),
            name,
            status,
            tag,
            back: t("workspace-settings-agent-access-back"),
            note: t("workspace-settings-agent-access-note"),
            editable,
            can_retire: may.manage,
            retire: t("workspace-settings-retire-agent"),
            answers: [
                t("workspace-settings-answer-allow"),
                t("workspace-settings-answer-ask"),
                t("workspace-settings-answer-deny"),
            ],
            reset: t("workspace-settings-agent-access-reset"),
            custom: t("workspace-settings-agent-access-set-here"),
            apps: Self::access_apps(&rows),
            // The keeper chooses the model on their own devices, which run it: a page runs none.
            models: (!RUNS_ON_HOST).then_some(models),
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
    pub(crate) fn moved(&self, tree: &Layout, person: &UserId) -> bool {
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
        // A record that could not be opened yet (one the workspace has not delivered to a page)
        // is asked for again, until it opens.
        let Some(record) = open.record.as_ref() else {
            return true;
        };
        self.shown.borrow().as_ref()
            != Some(&Seen {
                revision: record.revision(),
            })
    }

    /// The record of `workspace` as `person`, opened here when it is not yet,
    /// and opened again when the workspace's grants changed, so its decisions
    /// follow them.
    fn record(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
    ) -> Option<WorkspaceResource<Record>> {
        let workspace = workspace?;
        let workspace_id = workspace.id().to_string();
        let held = self.open.borrow();
        let current = held.as_ref().filter(|open| {
            open.workspace == workspace_id
                && open.person == *person
                && open.policy == workspace.policy
        });
        if let Some(record) = current.and_then(|open| open.record.clone()) {
            return Some(record);
        }
        drop(held);
        let (records, permissions) = self.host.open(workspace, person);
        let record = WorkspaceResource::<Record>::open(
            &records,
            OWNER,
            SETTINGS_APP,
            permissions,
            Some(self.wake.handle()),
        );
        *self.open.borrow_mut() = Some(Open {
            workspace: workspace_id,
            person: person.clone(),
            policy: workspace.policy.clone(),
            record: record.clone(),
        });
        record
    }

    /// Take up what the record owes this device while the tab is up: one
    /// record for every agent the workspace admitted before records were,
    /// and the keeper's model choice applied to this device's Settings.
    ///
    /// Nothing happens until the tab opened the record here: no other tab
    /// reads it.
    #[cfg(target_os = "linux")]
    pub(crate) fn settle(&self, tree: &Layout, person: &UserId) {
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
            .and_then(|open| open.record.clone());
        drop(held);
        let Some(record) = record else {
            return;
        };
        if self.settled.borrow().as_ref() == Some(&(workspace_id.clone(), record.revision())) {
            return;
        }
        let config = agent_provider_config::ProviderConfig::load();
        Self::record_admitted(workspace, person, &record, &config);
        Self::project_model(person, &record, &config);
        *self.settled.borrow_mut() = Some((workspace_id, record.revision()));
    }

    /// Make the record name every agent `person` admitted to `workspace`: one made for each of
    /// their admissions that has none, under the id its grants carry, so the roster, the names
    /// people read and the search find the agents that worked here before records were. Whoever
    /// admitted an agent keeps it, and only they make its record: another member's admission
    /// gets its record on a device of that member.
    /// An admission names no name of its own — its id is no name a person reads — so each is
    /// numbered, in id order, around the names the workspace already uses.
    /// A model chosen for the agent on this device before the record held one is its keeper's
    /// choice, and goes into the record with it.
    #[cfg(target_os = "linux")]
    fn record_admitted(
        workspace: &Workspace,
        person: &UserId,
        record: &WorkspaceResource<Record>,
        config: &agent_provider_config::ProviderConfig,
    ) {
        let state = record.get();
        let mut taken: Vec<String> = state.active().map(|agent| agent.name.clone()).collect();
        let mut number: u64 = 0;
        for (agent, _) in workspace.agent_members_at(tilo_clock::unix_now()) {
            if state.get(&Id::from_raw(agent.clone())).is_some() {
                continue;
            }
            // Its admission names who gave it: an agent admitted by someone is kept by them. A
            // service's membership is no agent here.
            let prefix = format!("agent-member:{agent}:");
            let keeper = workspace
                .policy
                .grants
                .iter()
                .filter(|grant| grant.id.as_str().starts_with(&prefix))
                .find_map(|grant| grant.granted_by.person_id().map(str::to_owned));
            // Whoever admitted it makes its record, on a device of their own: the record takes no
            // keeper a caller names, so another member's admission waits for that member.
            if keeper.is_none_or(|keeper| keeper != *person) {
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
            let model = agent_access::agent_models(config, &agent)
                .and_then(|models| models.chosen.map(|chosen| chosen.key()));
            let made = record.invoke(&Record::CREATE_AGENT.with(actions::CreateAgent {
                name,
                keeper: None,
                id: Some(agent.clone()),
                model,
                mcp_client: None,
            }));
            if let Err(refusal) = made {
                // The tab still lists the agent from its grants until Chat's
                // grants let the person make the record.
                tilo_failure::ignored!(
                    "the tab lists the agent from its grants until Chat's grants let the record be made",
                    tracing::debug!(%agent, %refusal, "an admitted agent has no record here yet")
                );
            }
        }
    }

    /// Apply the record's choice of model to this device's Settings, for the agents this person
    /// keeps: once per change of the record.
    #[cfg(target_os = "linux")]
    fn project_model(
        person: &UserId,
        record: &WorkspaceResource<Record>,
        config: &agent_provider_config::ProviderConfig,
    ) {
        let state = record.get();
        for agent in state.active() {
            if agent.keeper != *person {
                continue;
            }
            let held = agent_access::agent_models(config, &agent.id.0)
                .and_then(|models| models.chosen.map(|chosen| chosen.key()));
            if held == agent.model {
                continue;
            }
            let chosen = agent.model.as_ref().and_then(|model| {
                agent_access::agent_models(config, &agent.id.0)?
                    .offered
                    .into_iter()
                    .find(|option| &option.key() == model)
            });
            match agent_access::choose_agent_model(&agent.id.0, chosen) {
                Ok(_) => {
                    tracing::debug!(agent = %agent.id.0, "the keeper's model choice is applied here");
                }
                Err(error) => {
                    tracing::warn!(agent = %agent.id.0, %error, "the keeper's model choice is not applied here");
                }
            }
        }
    }

    /// Carry out an ask of the Agents tab for `person` in `workspace` (the one in front): making,
    /// dressing, bounding and retiring an agent are the record's own actions. `None` for an ask
    /// that is no agent's.
    pub(crate) fn answer(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
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
                // it and this person keeps it.
                match self.create(workspace, person, (name.as_str(), None), model) {
                    Ok(_) => Answered::Done,
                    Err(reason) => Answered::Refused(reason),
                }
            }
            // The review was of an agent the workspace no longer lists.
            Ask::RemoveAgent { agent } => shown(self.retire(workspace, person, agent), None),
            Ask::SetAgentModel { agent, model } => {
                shown(self.set_model(workspace, person, agent, model), Some(agent))
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
                shown(
                    self.set_limit(workspace, person, agent, limit_micros),
                    Some(agent),
                )
            }
            Ask::SetAgentAnswer {
                agent,
                scope,
                kind,
                answer,
            } => {
                let (Some(scope), Some(kind)) =
                    (AnswerScope::parse(scope), agent_access::kind_from_key(kind))
                else {
                    tracing::debug!(%scope, %kind, "no such answer: nothing changed");
                    return Some(Answered::Done);
                };
                let answer = if answer.is_empty() {
                    None
                } else {
                    let Some(answer) = tilo_action_policy::Answer::parse(answer) else {
                        tracing::debug!(%answer, "no such answer: nothing changed");
                        return Some(Answered::Done);
                    };
                    Some(answer)
                };
                shown(
                    self.set_answers(workspace, person, agent, &[(scope, kind, answer)]),
                    Some(agent),
                )
            }
            Ask::ResetAgentApp { agent, scope } => {
                let Some(scope) = AnswerScope::parse(scope) else {
                    tracing::debug!(%scope, "no such app: nothing changed");
                    return Some(Answered::Done);
                };
                let answers: Vec<_> = agent_access::KINDS
                    .into_iter()
                    .map(|kind| (scope.clone(), kind, None))
                    .collect();
                shown(
                    self.set_answers(workspace, person, agent, &answers),
                    Some(agent),
                )
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
        person: &UserId,
        (name, mcp_client): (&str, Option<&str>),
        model: &str,
    ) -> Result<(String, String), String> {
        let record = self
            .record(workspace, person)
            .ok_or_else(|| t("workspace-settings-agents-unavailable"))?;
        let name = tilo_resource::NonBlank::parse(name.trim())
            .ok_or_else(|| t("workspace-settings-agent-name-needed"))?;
        let id = record.get().free_id(name.as_str());
        record
            .invoke(&Record::CREATE_AGENT.with(actions::CreateAgent {
                name: name.clone(),
                keeper: None,
                id: Some(id.clone()),
                model: (!model.is_empty()).then(|| model.to_owned()),
                mcp_client: mcp_client.map(str::to_owned),
            }))
            .map_err(|error| error.to_string())?;
        if RUNS_ON_HOST && mcp_client.is_none() {
            record
                .invoke(&Record::SET_AGENT_ON_HOST.with(actions::SetAgentOnHost {
                    agent: Id::from_raw(id.clone()),
                    on_host: true,
                }))
                .map_err(|error| error.to_string())?;
        }
        Ok((id, name.as_str().to_owned()))
    }

    /// The active agent `person` keeps in `workspace` for the MCP client `client`, with the name
    /// the record keeps for it.
    #[cfg(target_os = "linux")]
    pub(crate) fn of_client(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
        client: &str,
    ) -> Option<(String, String)> {
        let record = self.record(workspace, person)?;
        let state = record.get();
        state
            .active()
            .find(|agent| agent.keeper == *person && agent.mcp_client.as_deref() == Some(client))
            .map(|agent| (agent.id.0.clone(), agent.name.clone()))
    }

    /// Retire the agent `agent`: it takes no new work, and what it did stays
    /// readable.
    ///
    /// # Errors
    /// The workspace's agents could not be opened here, or Chat's grants
    /// refuse the change.
    pub(crate) fn retire(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
        agent: &str,
    ) -> Result<(), String> {
        let record = self
            .record(workspace, person)
            .ok_or_else(|| t("workspace-settings-agents-unavailable"))?;
        record
            .invoke(&Record::RETIRE_AGENT.with(actions::RetireAgent {
                agent: Id::from_raw(agent.to_owned()),
            }))
            .map_err(|error| error.to_string())
    }

    /// Set what the agent `agent` may do in `workspace`, each answer for a kind of action at a scope
    /// (`None` follows its admission there again): the bound the workspace's list keeps for it, which every
    /// device and the workspace's host hold its calls to.
    ///
    /// # Errors
    /// The workspace's agents could not be opened here, or Chat's grants or the list refuse the
    /// bound.
    pub(crate) fn set_answers(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
        agent: &str,
        answers: &[(AnswerScope, ActionKind, Option<Answer>)],
    ) -> Result<(), String> {
        let agents = self
            .record(workspace, person)
            .ok_or_else(|| t("workspace-settings-agents-unavailable"))?;
        let id = Id::from_raw(agent.to_owned());
        let held = agents
            .get()
            .get(&id)
            .and_then(Agent::bound_rules)
            .unwrap_or_default();
        let root =
            workspace.map_or_else(String::new, |workspace| workspace.policy.workspace.clone());
        let bound = agent_access::bound_with(&held, &root, answers)
            .iter()
            .map(Bound::of)
            .collect();
        agents
            .invoke(&Record::SET_AGENT_BOUND.with(actions::SetAgentBound { agent: id, bound }))
            .map_err(|error| error.to_string())
    }

    /// Choose the model the agent `agent` plans with, as its keeper.
    ///
    /// # Errors
    /// The workspace's agents could not be opened here, Chat's grants refuse
    /// the change, or the person does not keep the agent.
    pub(crate) fn set_model(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
        agent: &str,
        model: &str,
    ) -> Result<(), String> {
        let record = self
            .record(workspace, person)
            .ok_or_else(|| t("workspace-settings-agents-unavailable"))?;
        record
            .invoke(&Record::SET_AGENT_MODEL.with(actions::SetAgentModel {
                agent: Id::from_raw(agent.to_owned()),
                model: (!model.is_empty()).then(|| model.to_owned()),
            }))
            .map_err(|error| error.to_string())
    }

    /// Set what the agent `agent` may spend a day, as its keeper: `None`
    /// spends unmetered.
    ///
    /// # Errors
    /// The workspace's agents could not be opened here, Chat's grants refuse
    /// the change, or the person does not keep the agent.
    pub(crate) fn set_limit(
        &self,
        workspace: Option<&Workspace>,
        person: &UserId,
        agent: &str,
        limit_micros: Option<u64>,
    ) -> Result<(), String> {
        let record = self
            .record(workspace, person)
            .ok_or_else(|| t("workspace-settings-agents-unavailable"))?;
        record
            .invoke(
                &Record::SET_AGENT_SPEND_LIMIT.with(actions::SetAgentSpendLimit {
                    agent: Id::from_raw(agent.to_owned()),
                    limit_micros,
                }),
            )
            .map_err(|error| error.to_string())
    }
}

/// The row of the moderation agent the workspace's server offers: on while the workspace holds its
/// grant, and turned by whoever may grant.
fn moderation_row(workspace: &Workspace, may: May) -> sheet::Moderation {
    let on = domain::moderates(workspace);
    sheet::Moderation {
        name: t("workspace-settings-moderation"),
        detail: t("workspace-settings-moderation-detail"),
        on,
        switchable: may.manage,
        state: t(if on {
            "workspace-settings-moderation-on"
        } else {
            "workspace-settings-moderation-off"
        }),
    }
}

/// Today, as a day's counter keeps it: this device's clock, which is its keeper's on the device
/// that runs the agent.
fn today() -> String {
    ui::clock::today().to_string()
}

/// What an agent may spend a day, as its review shows it: its keeper's limit,
/// what it spent of it, and — for its keeper, `keeps`, `keeper` being who
/// keeps it — the way to set it.
fn limit_of(
    agent: &tilo_app::tilo_workspace_agent::Agent,
    keeps: bool,
    keeper: &str,
) -> sheet::Limit {
    sheet::Limit {
        title: t("workspace-settings-agent-limit"),
        summary: limit_summary(agent.spend_limit_micros),
        today: t_args(
            "workspace-settings-agent-limit-today",
            &[("spent", ui::format::money(agent.spent_on(&today())).into())],
        ),
        note: if keeps {
            t("workspace-settings-agent-limit-keeper-you")
        } else {
            t_args(
                "workspace-settings-agent-limit-keeper",
                &[("name", keeper.into())],
            )
        },
        placeholder: t("workspace-settings-agent-limit-placeholder"),
        set: t("workspace-settings-agent-limit-set"),
        can_choose: keeps,
    }
}

/// What `agent` spends against its keeper's day, as its row reads it: the money it spent today
/// beside what it may, or only the money where its keeper set no limit.
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

/// Who keeps `agent` , as its row reads it: the person whose devices run it.
fn keeper_line(
    names: &PeopleNames,
    workspace: &Workspace,
    record: &WorkspaceResource<Record>,
    keeper: &str,
) -> String {
    if keeper == record.me().id() {
        return t("workspace-settings-agent-keeper-you");
    }
    let Some(member) = MemberId::parsed_person(keeper) else {
        return keeper.to_owned();
    };
    t_args(
        "workspace-settings-agent-keeper",
        &[(
            "name",
            domain::member_row_name(names, workspace, record.me().id(), &member).into(),
        )],
    )
}

/// The model `agent` plans with, as its row reads it: the one its keeper chose, or that none is
/// chosen yet.
fn model_line(
    config: &agent_provider_config::ProviderConfig,
    agent: &str,
    chosen: Option<&str>,
) -> String {
    let offered = agent_access::agent_models(config, agent);
    let label = chosen.map_or_else(
        || t("workspace-settings-agent-model-none"),
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
