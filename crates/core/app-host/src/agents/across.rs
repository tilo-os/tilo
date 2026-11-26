//! What a turn of a person's own assistant is told of their other workspaces.
//!
//! A person's assistant lives in the workspace that is theirs alone, and a turn of it reaches
//! that workspace's resources and nothing of any other workspace. [`TurnElsewhere`] is the
//! one way past that, a provider of the turn's own ([`TurnProvider`]): it lists the other
//! workspaces its person is a member of on this server, each with whether the assistant may
//! read there. It reads nothing in any of them and changes nothing anywhere.
//!
//! Whom it answers, decided again at every call:
//!
//! - a turn this server carries out ([`Workspaces::asked_turn`]): never a sub-agent's,
//!   and an agent the workspace's answers name the action for ([`agent_policy_allows`]);
//! - in a workspace that is its person's own: one this server keeps as theirs
//!   ([`Workspaces::owns`]), or, on a server that keeps none for people, one they are the
//!   owner of;
//! - where they are the only member not removed. Whoever is in that workspace holds the
//!   conversation, so from the moment somebody else is, a call is refused in words that say
//!   why. A turn of theirs that begins after that is still named the list where this server
//!   keeps the workspace as theirs, so that its call says so. On a server that keeps no
//!   workspaces for people nothing tells a person's own workspace that gained a member from a
//!   shared one they own, and such a turn is named none.
//!
//! Every other caller is answered what an unknown action is, and its listing of everything
//! names no such provider.
//!
//! What the list says of each workspace:
//!
//! - `always`: the person let reading there stay allowed for this agent
//!   (a grant of the workspace's policy, [`Workspaces::granted`]);
//! - `not_asked`: they are asked before anything is read there;
//! - `no_device`: the workspace admits them on no device of their own now. A device this
//!   server placed itself for a workspace's window is certified in a person's name and is
//!   often the newest they have there, and this server's own device is a member of every
//!   workspace it hosts. Neither is ever the device a read for a person acts as, so a
//!   person with no other device there is on none. A server that was not told which devices
//!   it placed (`knowing_placed` of this server's agents) cannot tell them apart and says the
//!   same of every workspace;
//! - `another_server`: the person's own list of their workspaces names it and this server
//!   does not host it ([`Workspaces::kept_elsewhere`]).
//!
//! A workspace the person is no member of is not in the list, whether it exists or not.

use std::sync::Arc;

use serde_json::{Value, json};
use tilo_actions::remote::ActionCallError as NotTaken;
use tilo_actions::remote::{
    Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeRequest, Pending,
    RemoteActionTarget,
};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionFailure, ActionKind, ActionOutput, InvocationUpdate,
};
use tilo_workspace_agent::Agent;

use super::turn_provider::{Ending, Rows};
use super::{Turn, TurnProvider, TurnScope, WorkspaceId};
use tilo_workspace_service::environment::EnvironmentRegistry;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::hosted_use::agent_policy_allows;

/// The provider a turn's tools find its person's other workspaces under.
const PROVIDER: &str = "tilo.other-workspaces";
/// The one resource it holds: the person's other workspaces.
const MINE: &str = "workspaces";
/// The read of them.
const LIST: &str = "list";
/// Reading in one of them, as a permission that stays names it.
const READ: &str = "read";
/// The code of a call refused in words.
const REFUSED: &str = "refused";
/// The role of a workspace's owner, as its membership names it.
const OWNER: &str = "owner";

/// The person let reading in the workspace stay allowed.
const ALWAYS: &str = "always";
/// The person is asked before anything is read in the workspace.
const NOT_ASKED: &str = "not_asked";
/// The workspace admits the person on no device of their own now.
const NO_DEVICE: &str = "no_device";
/// Another server keeps the workspace.
const ANOTHER_SERVER: &str = "another_server";

/// What a call is told once its person's own workspace has somebody else in it.
const NOT_ALONE: &str = "The workspace this conversation is in has another member now, and \
     whoever is in it can read this conversation. So nothing of your person's other \
     workspaces is listed or read from here. Tell them that, and that it holds for as long \
     as somebody else is a member of this workspace.";

/// What one turn is told of its person's other workspaces.
pub(super) struct TurnElsewhere {
    workspaces: Workspaces,
    /// The devices this server placed itself, where it was told them.
    placed: Option<EnvironmentRegistry>,
    /// The turn's own workspace.
    workspace: String,
    workspace_id: WorkspaceId,
    /// The turn's agent, as the workspace's list held it when the turn began.
    agent: Agent,
    /// Whether the turn's listing names the provider: this answered the turn when it began,
    /// or would have but for somebody else in its person's own workspace, where this server
    /// keeps that workspace as theirs. No other turn's listing names it.
    offered: bool,
}

/// Why a caller is told nothing of its person's other workspaces.
enum Refusal {
    /// It is no turn this answers: it is answered what an unknown action is.
    Unknown,
    /// Its person's own workspace has somebody else in it now.
    NotAlone,
}

/// Make `scope`'s service answer as well for the other workspaces of `turn`'s person, where
/// `placed` holds the devices this server placed itself.
pub(super) async fn join(
    workspaces: &Workspaces,
    placed: Option<&EnvironmentRegistry>,
    turn: &Turn,
    scope: &mut TurnScope,
) {
    let mut own = TurnElsewhere {
        workspaces: workspaces.clone(),
        placed: placed.cloned(),
        workspace: scope.workspace.clone(),
        workspace_id: scope.workspace_id,
        agent: turn.agent.clone(),
        offered: false,
    };
    let caller = scope.reply(turn).caller;
    own.offered = match own.whose(&caller, LIST).await {
        Ok(_) => true,
        // Where this server keeps the workspace as that person's own, a turn of theirs that
        // begins once somebody else is in it is still named the list, so that its call says
        // why nothing is listed. Nobody else's turn is named it.
        Err(Refusal::NotAlone) => workspaces.keeps_own_workspaces(),
        Err(Refusal::Unknown) => false,
    };
    let service = &mut scope.provider;
    *service = service.clone().answering(Arc::new(own));
}

impl TurnElsewhere {
    /// The person `caller`'s turn acts for, where it is one this answers `action` to: a turn
    /// they asked of the agent, in a workspace that is theirs and holds nobody else now.
    async fn whose(&self, caller: &Caller, action: &str) -> Result<[u8; 32], Refusal> {
        if self.agent.is_sub() {
            return Err(Refusal::Unknown);
        }
        let Ok(asked) = self
            .workspaces
            .asked_turn_of(self.workspace_id, caller)
            .await
        else {
            return Err(Refusal::Unknown);
        };
        let Some((agent, owner)) = asked.agent.as_agent() else {
            return Err(Refusal::Unknown);
        };
        let policy = self
            .workspaces
            .agent_policy(&self.workspace, agent, owner.as_str())
            .await;
        let target = RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(MINE, action),
        };
        if asked.agent != self.agent.member()
            || !agent_policy_allows(
                policy.as_ref(),
                Some((agent, owner.as_str())),
                &self.workspace,
                &target,
            )
        {
            return Err(Refusal::Unknown);
        }
        let person = hex::encode(asked.person);
        let Ok((_, people)) = self.workspaces.membership(&self.workspace).await else {
            return Err(Refusal::Unknown);
        };
        let theirs = if self.workspaces.keeps_own_workspaces() {
            self.workspaces.owns(&asked.person, &self.workspace)
        } else {
            people
                .get(&person)
                .is_some_and(|mark| !mark.removed && !mark.banned && mark.role == OWNER)
        };
        if !theirs {
            return Err(Refusal::Unknown);
        }
        if people
            .iter()
            .any(|(who, mark)| *who != person && !mark.removed)
        {
            return Err(Refusal::NotAlone);
        }
        Ok(asked.person)
    }

    /// The device of `person`'s own that a read for them in the workspace `there` acts as:
    /// the newest that workspace admits them on now that is neither this server's own device
    /// nor one this server placed itself ([`Workspaces::persons_own_device`]). None where they
    /// have no such device there, and none where these agents were not told what this server
    /// placed. What the agents were told is asked once more here.
    async fn device_there(&self, there: WorkspaceId, person: [u8; 32]) -> Option<[u8; 32]> {
        let placed = self.placed.as_ref()?;
        let device = self.workspaces.persons_own_device(there, person).await?;
        let theirs = device != self.workspaces.device_cert().device_public_key
            && placed.of(&device, tilo_clock::unix_now_ns()).is_none();
        theirs.then_some(device)
    }

    /// What a grant to read in the workspace `there` names: this agent, asked in the turn's
    /// own workspace, reading in that one workspace.
    fn reading_in(&self, there: &str) -> tilo_action_policy::TreePath {
        tilo_action_policy::TreePath::workspace(&self.workspace)
            .app(PROVIDER)
            .kind(MINE)
            .resource(there)
            .action(READ)
    }

    /// The other workspaces `person` is a member of on this server, and those their own list
    /// says another server keeps, each with whether this agent may read there.
    async fn list(&self, person: [u8; 32]) -> Value {
        let me = hex::encode(person);
        let mut here = self.workspaces.list().await;
        here.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.workspace.cmp(&b.workspace))
        });
        let mut listed = Vec::new();
        for summary in here {
            if summary.workspace == self.workspace {
                continue;
            }
            let member = self
                .workspaces
                .membership(&summary.workspace)
                .await
                .is_ok_and(|(_, people)| {
                    people
                        .get(&me)
                        .is_some_and(|mark| !mark.removed && !mark.banned && !mark.role.is_empty())
                });
            if !member {
                continue;
            }
            let reading = if self
                .device_there(summary.workspace_id, person)
                .await
                .is_none()
            {
                NO_DEVICE
            } else if self
                .workspaces
                .granted(
                    &self.workspace,
                    &self.agent.member(),
                    &self.reading_in(&summary.workspace),
                )
                .await
            {
                ALWAYS
            } else {
                NOT_ASKED
            };
            listed.push(json!({
                "workspace": summary.workspace,
                "name": summary.name,
                "reading": reading,
            }));
        }
        for (workspace, name) in self.workspaces.kept_elsewhere(&person).await {
            listed.push(json!({
                "workspace": workspace,
                "name": name,
                "reading": ANOTHER_SERVER,
            }));
        }
        json!({
            "workspaces": listed,
            "reading": {
                "always": "Your person allowed reading there for good.",
                "not_asked": "Your person is asked before anything is read there.",
                "no_device": "That workspace admits your person on no device of their own \
                              now, so nothing is read there for them.",
                "another_server": "Another server keeps that workspace. Nothing of it is \
                                   reached from here.",
            },
        })
    }

    /// Take the call `request` asks for as `caller`: how it ended, or why this server did
    /// not take it.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, NotTaken> {
        let invocation = request.invocation;
        let address = &request.target.address;
        if address.resource != MINE || address.action != LIST {
            return Err(NotTaken::UnknownAction);
        }
        let person = match self.whose(caller, LIST).await {
            Ok(person) => person,
            Err(Refusal::Unknown) => return Err(NotTaken::UnknownAction),
            Err(Refusal::NotAlone) => {
                return Ok(InvocationUpdate::Failed {
                    invocation,
                    error: ActionFailure {
                        code: REFUSED.to_owned(),
                        message: NOT_ALONE.to_owned(),
                        details: None,
                        retryable: false,
                    },
                });
            }
        };
        Ok(InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(self.list(person).await),
                resources: Vec::new(),
            },
        })
    }
}

impl TurnProvider for TurnElsewhere {
    /// The provider, as a listing of everything names it beside the workspace's resources:
    /// to a turn this did not answer when it began, not at all.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        if !self.offered {
            return Vec::new();
        }
        vec![CatalogueProvider {
            provider: PROVIDER.to_owned(),
            app_id: PROVIDER.to_owned(),
            workspace: self.workspace.clone(),
            document_id: String::new(),
            browsable: true,
        }]
    }

    /// The rows `query` asks for, when it asks this provider.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (self.offered && query.provider.as_deref() == Some(PROVIDER))
            .then(|| Pending::ready(Ok(rows())))
    }

    /// Everything asked of this provider is this one's to answer, also what it refuses.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == PROVIDER
    }

    /// A turn's caller changes when its person writes again while it works, so a call is
    /// taken as the caller of the tool call that asked for it.
    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}

/// The rows a catalogue offers an agent: the one read of its person's other workspaces.
fn rows() -> Vec<CatalogueRow> {
    let mut contract = ActionContract {
        kind: ActionKind::Read,
        ..ActionContract::default()
    };
    // A turn's own: no person picks it from a list.
    contract.set_audience(tilo_actions::contract::Audience::Agents);
    vec![CatalogueRow::for_action(
        PROVIDER,
        PROVIDER,
        ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(MINE, LIST),
            resource_kind: MINE.to_owned(),
            resource_label: "Other workspaces".to_owned(),
            action_label: "List workspaces".to_owned(),
            keywords: Vec::new(),
            icon: None,
            description: Some(
                "The other workspaces your person is a member of on this server, each by its \
                 name and id, with whether you may read there: always, not asked yet, on no \
                 device of their own there, or kept by another server. It reads nothing in \
                 any of them."
                    .to_owned(),
            ),
            locus: tilo_actions::ExecutionLocus::ServerPreferred,
            kind: tilo_actions::OperationKind::Effect,
            contract,
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        },
    )]
}
