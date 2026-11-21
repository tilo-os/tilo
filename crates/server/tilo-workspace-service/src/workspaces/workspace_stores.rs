//! What a workspace keeps on the host that serves it beside its records: its
//! mail and calendar accounts with their sign-ins, its model providers and
//! their keys, the services it connected, what its agents learned, and the
//! items of its vault, sealed under a key of the workspace's own. One set per
//! workspace, for all its members, kept under `workspaces/<workspace>/` in the
//! host's state, beside the workspace's asks and grants.
//!
//! The workspace's policy decides who reaches them, as it decides every app's
//! records ([`tilo_action_policy::decide`]). Each store stands at its app's
//! node ([`Store`]): the mail accounts at Mail's, the calendars at
//! Calendar's, the vault's items at Passwords' vault. A member reaches them
//! through their role (an Editor uses the mail accounts and the vault's
//! items, a Viewer reads them), an agent or an API key through its grants,
//! and an owner who wants one closed says so with a grant like any other. A
//! caller the workspace does not have is told what a caller asking for
//! something that does not exist is told; one it has but whose grants do not
//! reach is refused.
//!
//! What stays a person's own (their formats and zone, what may interrupt
//! them) is kept apart, under `people/<person>/`, and no workspace reaches it.
//! A host that kept these stores per person converts them as it opens
//! ([`Workspaces::move_people_stores`]): each person's store goes into the
//! workspaces they own here, and nowhere they are merely a member.

use std::path::{Path, PathBuf};

use tilo_action_policy::{ActionKind, Answer, Context, Role, TreePath};
use tilo_actions::remote::{ActionCallError, Caller};
use tilo_server_protocol::certify::PendingConfirmation;
use tilo_user_identity::DeviceCert;

use super::{WorkspaceId, Workspaces};

/// What a host kept for a person beside their own settings, before a
/// workspace kept it: everything under `people/<person>/` but these.
/// How long the person has to answer a question about a use.
const QUESTION_WAIT_NS: u64 = 2 * 60 * 1_000_000_000;

const PERSON_OWN: [&str; 2] = ["formats.json", "telling"];

/// Where a store stands in the workspace's policy: an app's node, and the
/// kind of resource it is in that app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Store {
    app: &'static str,
    kind: &'static str,
}

impl Store {
    /// The workspace's mail accounts, in Mail.
    pub(super) const MAIL: Self = Self {
        app: tilo_app_id::apps::MAIL,
        kind: tilo_actions::remote::person_accounts::PROVIDER,
    };
    /// The workspace's calendar accounts, in Calendar: only agents' hosts keep them.
    #[cfg(feature = "agents")]
    pub(super) const CALENDARS: Self = Self {
        app: tilo_app_id::apps::CALENDAR,
        kind: tilo_actions::remote::person_accounts::PROVIDER,
    };
    /// The items of the workspace's vault, in Passwords.
    pub(super) const VAULT: Self = Self {
        app: tilo_app_id::apps::PASSWORDS,
        kind: "passwords.workspace-vault",
    };
    /// The workspace's model providers and their keys.
    pub(super) const MODELS: Self = Self {
        app: tilo_actions::remote::person_models::PROVIDER,
        kind: tilo_actions::remote::person_models::RESOURCE,
    };
    /// The services the workspace connected.
    pub(super) const CONNECTIONS: Self = Self {
        app: tilo_actions::remote::person_connections::PROVIDER,
        kind: tilo_actions::remote::person_connections::KIND,
    };
    /// What the workspace's agents learned.
    pub(super) const LEARNED: Self = Self {
        app: super::person_learned::PROVIDER,
        kind: super::person_learned::RESOURCE,
    };

    /// The node `action` of this store stands at in `workspace`.
    pub(super) fn node(self, workspace: &str, action: &str) -> TreePath {
        TreePath::workspace(workspace)
            .app(self.app)
            .kind(self.kind)
            .rest()
            .action(action)
    }
}

/// Who asks of a workspace's stores, once its policy let them.
#[derive(Clone, Debug)]
pub(super) struct Reach {
    /// The workspace whose stores they reach.
    pub workspace: WorkspaceId,
    /// The person behind the caller: the person themselves, or the person
    /// whose agent or key it is.
    pub person: [u8; 32],
    /// The agent's name, as its person knows it, when an agent asks.
    pub agent: Option<String>,
    /// What the policy answered: [`Answer::Allow`], or [`Answer::Ask`] for an
    /// agent its person is asked about first.
    pub answer: Answer,
}

impl Reach {
    /// The name a use is remembered under: the agent's, or none for the
    /// person themselves.
    pub(super) fn by(&self) -> String {
        self.agent.clone().unwrap_or_default()
    }
}

impl Workspaces {
    /// Where this host keeps what `workspace` keeps on it beside its records:
    /// its accounts, providers, connections and vault, with the key the vault
    /// is sealed under where there is no keyring.
    pub(super) fn workspace_root(&self, workspace: &str) -> PathBuf {
        super::person_asking::workspace_store_dir(&self.inner.dir, workspace)
    }

    /// Whether `caller`, on the device `certificate` certifies, may do
    /// `effect` with `action` of `store` in the workspace `workspace_id`
    /// names, as the workspace's policy decides it now. A person acting as
    /// themselves is asked nothing: their invocation is their answer. An
    /// agent its person's session was handed to is held to its delegation
    /// too: it reads, and a change asks its person ([`Answer::Ask`]) unless
    /// they answered that very action in advance.
    ///
    /// # Errors
    /// [`ActionCallError::UnknownAction`] for a caller the workspace does not
    /// have, on a device it does not certify, or a workspace not governed
    /// here; [`ActionCallError::NotPermitted`] for one whose grants do not
    /// reach.
    pub(super) async fn reaching(
        &self,
        (workspace_id, caller, certificate): (
            Option<WorkspaceId>,
            &Caller,
            Option<&DeviceCert>,
        ),
        store: Store,
        action: &str,
        effect: ActionKind,
    ) -> Result<Reach, ActionCallError> {
        let workspace = workspace_id.ok_or(ActionCallError::UnknownAction)?;
        let acting = caller.acting();
        if acting.app().is_some() {
            return Err(ActionCallError::UnknownAction);
        }
        let certificate = certificate
            .filter(|certificate| {
                caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
            })
            .ok_or(ActionCallError::UnknownAction)?;
        let governed = self
            .held(workspace)
            .await
            .ok_or(ActionCallError::UnknownAction)?;
        let now = tilo_clock::wall();
        let access = governed
            .lock()
            .await
            .peer()
            .permissions(certificate.device_public_key, now)
            .map_err(|_| ActionCallError::UnknownAction)?;
        // An agent handed its person's session acts as that agent, within
        // what its person may.
        let access = if acting.is_agent() && caller.is_session_delegated() {
            access
                .as_acting(acting.clone())
                .ok_or(ActionCallError::UnknownAction)?
        } else {
            access
        };
        let node = store.node(access.workspace(), action);
        let verdict = access.verdict(&node, effect, &Context::at_time(now));
        let answer = if acting.person_id().is_some() {
            // The person acting as themselves is the one their invocation
            // asks: nothing is put to them.
            if verdict.answer == Answer::Ask {
                Answer::Allow
            } else {
                verdict.answer
            }
        } else if acting.is_agent() && effect != ActionKind::Read {
            // An agent reads freely. A change it makes is put to its person
            // first, unless they answered that very action in advance.
            verdict.parts.last().map_or(verdict.answer, |agent| {
                verdict
                    .answer
                    .min(tilo_action_policy::agent_answers::held_to_contract(agent, true))
            })
        } else {
            verdict.answer
        };
        if answer == Answer::Deny {
            return Err(ActionCallError::NotPermitted);
        }
        let agent = acting.is_agent().then(|| {
            caller
                .agent_name()
                .or_else(|| acting.agent_id())
                .unwrap_or_default()
                .to_owned()
        });
        Ok(Reach {
            workspace,
            person: certificate.user_public_key,
            agent,
            answer,
        })
    }

    /// [`Self::reaching`], carried through: where the policy asks the
    /// caller's person first, they are asked on their approving devices,
    /// shown what the action is and where, never its arguments (which may
    /// hold a secret), and only their allowing it lets it go on.
    ///
    /// # Errors
    /// As [`Self::reaching`], or the person did not allow it.
    pub(super) async fn reaching_allowed(
        &self,
        asked: (Option<WorkspaceId>, &Caller, Option<&DeviceCert>),
        store: Store,
        action: &str,
        effect: ActionKind,
    ) -> Result<Reach, ActionCallError> {
        let reach = self.reaching(asked, store, action, effect).await?;
        self.allowed(reach, asked.1, store, action, effect).await
    }

    /// `reach`, once its person allowed it where the policy asks them first:
    /// the second half of [`Self::reaching_allowed`], for a caller that looks
    /// at what is asked before it decides whether the question is its own.
    ///
    /// # Errors
    /// The person did not allow it.
    pub(super) async fn allowed(
        &self,
        mut reach: Reach,
        caller: &Caller,
        store: Store,
        action: &str,
        effect: ActionKind,
    ) -> Result<Reach, ActionCallError> {
        if reach.answer == Answer::Ask {
            let agent = caller
                .acting()
                .agent_id()
                .ok_or(ActionCallError::NotPermitted)?;
            let place = self.workspace_name(reach.workspace).await;
            let question = PendingConfirmation {
                id: super::actions::agents::question_id(),
                agent_id: agent.to_owned(),
                agent_label: reach.by(),
                title: String::new(),
                action_label: action.replace('-', " "),
                resource_label: place.unwrap_or_else(|| store.app.to_owned()),
                arguments: Vec::new(),
                risk: "sensitive".into(),
                kind: match effect {
                    ActionKind::Read => "read",
                    ActionKind::Undoable => "undoable",
                    _ => "outside",
                }
                .into(),
                undoable: effect == ActionKind::Undoable,
                workspace: reach.workspace.to_string(),
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(QUESTION_WAIT_NS),
                page: None,
                ask: None,
                allow_always: false,
            };
            self.ask_person(reach.person, question).await?;
            reach.answer = Answer::Allow;
        }
        Ok(reach)
    }

    /// Whether the person `person` may do `effect` with `action` of `store`
    /// in `workspace` now, as its policy decides for them: what a turn this
    /// server carries out for them reaches, with no device of theirs behind
    /// it. A policy that asks them has not refused them: the turn waits on
    /// their answer for that one change, and this is where the change is
    /// weighed again after it.
    pub(super) async fn person_reaches(
        &self,
        workspace: WorkspaceId,
        person: &[u8; 32],
        store: Store,
        action: &str,
        effect: ActionKind,
    ) -> bool {
        let Some(policy) = self.policy_of(&workspace.to_string()).await else {
            return false;
        };
        let person = hex::encode(person);
        if policy.active_role(&person).is_none() {
            return false;
        }
        let node = store.node(&policy.workspace, action);
        let access = tilo_action_policy::MemberAccess::person(policy, person.as_str());
        access.answer(&node, effect, &Context::at_time(tilo_clock::wall())) != Answer::Deny
    }

    /// Move what this host kept per person before its workspaces kept it:
    /// each `people/<person>/` store goes into every workspace governed here
    /// that the person owns (by its governance, or as one of their own
    /// here), its sealed secrets sealed again under that workspace's key, and
    /// then leaves the person's folder, where only what is theirs alone
    /// stays. A workspace they are merely a member of gets nothing of it. A
    /// workspace that keeps a store already keeps it as it is.
    pub(super) async fn move_people_stores(&self) {
        let people = self.inner.dir.join("people");
        let Ok(folders) = std::fs::read_dir(&people) else {
            return;
        };
        let owned_index = owned_index(&self.inner.dir);
        let governed: Vec<String> = self.inner.workspaces.lock().await.keys().cloned().collect();
        for folder in folders.flatten() {
            let from = folder.path();
            let Some(person) = person_of(&from) else {
                continue;
            };
            if !holds_a_store(&from) {
                continue;
            }
            let mut owned: Vec<String> = owned_index
                .get(&hex::encode(person))
                .cloned()
                .unwrap_or_default();
            for workspace in &governed {
                if self.member_role(workspace, person).await == Some(Role::Owner)
                    && !owned.contains(workspace)
                {
                    owned.push(workspace.clone());
                }
            }
            owned.retain(|workspace| governed.contains(workspace));
            let mut moved = true;
            for workspace in &owned {
                if let Err(error) = move_store(&from, &self.workspace_root(workspace), workspace) {
                    moved = false;
                    tracing::error!(%workspace, %error, "a person's store did not move into their workspace");
                }
            }
            // A person who owns no workspace here keeps their store until one is.
            if moved && !owned.is_empty() {
                forget_moved(&from);
                tracing::info!(person = %hex::encode(person), workspaces = owned.len(), "a person's store moved into the workspaces they own");
            }
        }
    }
}

/// Whose folder `folder` under `people/` is.
fn person_of(folder: &Path) -> Option<[u8; 32]> {
    let name = folder.file_name()?.to_str()?;
    let mut person = [0; 32];
    hex::decode_to_slice(name, &mut person).ok()?;
    Some(person)
}

/// Whether the person's folder `from` keeps anything a workspace keeps now.
fn holds_a_store(from: &Path) -> bool {
    std::fs::read_dir(from).into_iter().flatten().flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| !PERSON_OWN.contains(&name))
    })
}

/// The own workspaces this host's index names for each person (hex root),
/// as `own-workspaces.json` keeps them, whether or not this host offers own
/// workspaces this run.
fn owned_index(dir: &Path) -> std::collections::BTreeMap<String, Vec<String>> {
    let Ok(bytes) = std::fs::read(dir.join(super::own::INDEX)) else {
        return std::collections::BTreeMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        tracing::warn!(%error, "the index of own workspaces does not read: nothing is moved by it");
        std::collections::BTreeMap::new()
    })
}

/// Copy the person's store at `from` into the store of `workspace` at `to`:
/// the vault's secrets sealed again under `to`'s own key, what was learned
/// in that workspace, and every other file as it is. Who the person named for
/// which account is not copied: a workspace's grants say what its agents
/// reach. What `to` keeps already stays as it is.
fn move_store(from: &Path, to: &Path, workspace: &str) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|error| error.to_string())?;
    let vault = accounts::vault_dir_in(from);
    let skip = [
        vault.clone(),
        from.join(LEARNED),
        from.join("mail").join("assistants.json"),
    ];
    copy_tree(from, to, &skip)?;
    let learned = Path::new(LEARNED).join(format!("{workspace}.automerge"));
    if from.join(&learned).exists() && !to.join(&learned).exists() {
        std::fs::create_dir_all(to.join(LEARNED)).map_err(|error| error.to_string())?;
        std::fs::copy(from.join(&learned), to.join(&learned))
            .map_err(|error| error.to_string())?;
    }
    if vault.join("vault.enc").exists() {
        accounts::SecretVault::reseal_into(from, to)?;
    }
    Ok(())
}

/// The folder of a store that keeps what agents learned, one file a
/// workspace.
const LEARNED: &str = "learned";

/// Copy every regular file below `from` that is not the person's own, nor one
/// of `skip`, to the same place below `to`, keeping a file `to` has already.
/// What a store keeps while its host runs, a socket it talks to itself over,
/// is no part of the store and is left behind.
fn copy_tree(from: &Path, to: &Path, skip: &[PathBuf]) -> Result<(), String> {
    let mut pending = vec![from.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if skip.contains(&path) || (dir == from && is_person_own(&path)) {
                continue;
            }
            let below = path.strip_prefix(from).map_err(|error| error.to_string())?;
            let target = to.join(below);
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() {
                std::fs::create_dir_all(&target).map_err(|error| error.to_string())?;
                pending.push(path);
            } else if kind.is_file() && !target.exists() {
                std::fs::copy(&path, &target).map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

fn is_person_own(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| PERSON_OWN.contains(&name))
}

/// Take what moved out of the person's folder `from`, leaving what is
/// theirs alone.
fn forget_moved(from: &Path) {
    for entry in std::fs::read_dir(from).into_iter().flatten().flatten() {
        let path = entry.path();
        if is_person_own(&path) {
            continue;
        }
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = removed {
            tracing::warn!(path = %path.display(), %error, "what moved into a workspace stays in its person's folder too");
        }
    }
    // A folder with nothing of the person's own left goes.
    let _ = std::fs::remove_dir(from);
}
