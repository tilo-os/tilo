//! Lasting agent permissions and pending confirmations, unified on
//! `tilo-action-policy` workspace grants and [`super::AsksPeople`].
//!
//! Nothing is persisted under a per-person server disk directory: lasting
//! allowances are real [`tilo_action_policy::Grant`]s scoped to the workspace
//! the action acts in, and interactive approvals route through the server's
//! single pending-confirmation path ([`Workspaces::ask_person_confirmation`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_action_policy::agent_answers::AnswerScope;
use tilo_action_policy::{
    ActionKind, Answer, Context, Grant, GrantId, Grantee, KindSet, TreePath, WorkspacePolicy,
    decide,
};
use tilo_actions::remote::person_asking::{
    self, AgentName, Asked, AskedBy, Call, MAX_NAME_BYTES, MAX_WORDS_BYTES, Permission,
    Permissions, Source, TakeBack, Waiting, WaitingQuestions, Words,
};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_user_identity::DeviceCert;

use super::person_settings::{answer, argument};
use super::{GovernedWorkspace, Workspaces, WorkspacesInner};

pub mod kept_asks;

/// The most permissions that stand for one person at a time.
pub const MAX_PERMISSIONS: usize = 256;

const NOT_NAMED: &str = "A lasting permission names its agent, its action and its workspace";
const TOO_MANY: &str = "Too many permissions stand already: take one back first";
const NONE_STANDS: &str = "No such permission stands";
const APPROVAL_ID_PREFIX: &str = "approval:";

pub(super) enum ApprovalChange<'a> {
    Keep(&'a Grant),
    Revoke(&'a [GrantId]),
}

#[derive(Default)]
struct WaitingIndex(Mutex<BTreeMap<String, Vec<(String, Waiting)>>>);

/// Root directory for per-workspace pending asks and grants (never `people/<person>/`).
pub(super) fn workspace_store_root(dir: &Path) -> PathBuf {
    dir.join("workspaces")
}

/// Directory for one workspace's pending asks and grants.
pub(super) fn workspace_store_dir(dir: &Path, workspace: &str) -> PathBuf {
    workspace_store_root(dir).join(hex::encode(workspace.as_bytes()))
}

fn is_grant_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn grant_path(dir: &Path, workspace: &str, id: &str) -> PathBuf {
    workspace_store_dir(dir, workspace)
        .join("grants")
        .join(format!("{id}.json"))
}

fn write_grant(dir: &Path, entry: &PolicyEntry) -> Result<(), String> {
    let bytes = serde_json::to_vec(entry).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &grant_path(dir, &entry.scope.workspace, entry.grant.id.as_str()),
        &bytes,
        0o600,
    )
    .map_err(|error| error.to_string())
}

fn load_grants(dir: &Path) -> BTreeMap<String, Vec<PolicyEntry>> {
    let mut by_workspace: BTreeMap<String, Vec<PolicyEntry>> = BTreeMap::new();
    let files = std::fs::read_dir(workspace_store_root(dir))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|ws| {
            std::fs::read_dir(ws.path().join("grants"))
                .into_iter()
                .flatten()
        })
        .flatten();
    for file in files {
        let path = file.path();
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(entry) = serde_json::from_slice::<PolicyEntry>(&bytes) else {
            continue;
        };
        if is_grant_id(entry.grant.id.as_str())
            && id_of(&entry.scope).as_deref() == Some(entry.grant.id.as_str())
        {
            by_workspace
                .entry(entry.scope.workspace.clone())
                .or_default()
                .push(entry);
        }
    }
    by_workspace
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PolicyEntry {
    person: [u8; 32],
    grant: Grant,
    scope: Asked,
    name: String,
    words: Words,
    since_ms: u64,
    asked_by: Option<AskedBy>,
    used_times: u64,
    last_used_ms: Option<u64>,
}

/// Workspace-scoped `tilo-action-policy` grants and pending asks, loaded once
/// from each workspace's store (never under `people/<person>/`).
#[derive(Default)]
struct Held {
    asks: kept_asks::KeptAskStore<kept_asks::KeptAsk>,
    waiting: WaitingIndex,
    /// Grants indexed by workspace id, loaded once from the workspace store.
    grants: Mutex<Option<BTreeMap<String, Vec<PolicyEntry>>>>,
}

type Holders = BTreeMap<PathBuf, (Weak<WorkspacesInner>, Arc<Held>)>;
static HOLDERS: Mutex<Holders> = Mutex::new(BTreeMap::new());

/// What a person's answer `always` lets stand as a workspace policy grant.
#[derive(Clone, Debug)]
pub struct Allowing {
    pub scope: Asked,
    pub name: String,
    pub words: Words,
    pub asked_by: Option<AskedBy>,
}

/// The `TreePath` node in `tilo-action-policy` corresponding to `scope`.
#[must_use]
pub fn policy_node(scope: &Asked) -> TreePath {
    let base = TreePath::workspace(&scope.workspace)
        .app(&scope.action.provider)
        .kind(&scope.action.kind);
    match &scope.on {
        Some(thing) => {
            let resource = if thing.document.is_empty() {
                thing.resource.clone()
            } else {
                format!("{}/{}", thing.document, thing.resource)
            };
            base.resource(resource).action(&scope.action.action)
        }
        None => AnswerScope::action(
            &scope.action.provider,
            &scope.action.kind,
            &scope.action.action,
        )
        .node(&scope.workspace),
    }
}

/// Build the `tilo-action-policy` [`Grant`] for `scope`.
#[must_use]
pub fn policy_grant(person: &[u8; 32], id: &str, scope: &Asked) -> Grant {
    Grant::new(
        id.to_owned(),
        Grantee::agent(&scope.agent),
        policy_node(scope),
        KindSet::ALL,
        Answer::Allow,
        Grantee::person(hex::encode(person)),
    )
}

fn id_of(scope: &Asked) -> Option<String> {
    let named = serde_json::to_vec(scope).ok()?;
    Some(blake3::hash(&named).to_hex().chars().take(32).collect())
}

fn governed_permission(person: &[u8; 32], scope: &Asked) -> Option<Grant> {
    if !scope.is_named() {
        return None;
    }
    let named = serde_json::to_vec(&(person, scope)).ok()?;
    let id = format!("{APPROVAL_ID_PREFIX}{}", blake3::hash(&named).to_hex());
    Some(Grant::new(
        id,
        Grantee::agent(&scope.agent),
        policy_node(scope),
        KindSet::OUTSIDE,
        Answer::Allow,
        Grantee::person(hex::encode(person)),
    ))
}

pub(super) fn is_scoped_approval(grant: &Grant) -> bool {
    grant.id.as_str().starts_with(APPROVAL_ID_PREFIX)
}

fn approval_in_policy(
    policy: &WorkspacePolicy,
    expected: &Grant,
    proposed: bool,
) -> Result<bool, String> {
    let person = expected.granted_by.person_id().ok_or(NOT_NAMED)?;
    let context = Context::at(tilo_clock::unix_now_ms());
    let (node, kind) = (&expected.node, ActionKind::Outside);
    if policy.active_role(person).is_none()
        || policy.party(person).decide(node, kind, &context).answer == Answer::Deny
    {
        return Err("workspace policy does not allow the person to approve this action".to_owned());
    }
    let grants = policy
        .grants
        .iter()
        .filter(|grant| !is_scoped_approval(grant) || grant.granted_by == expected.granted_by);
    let decision = decide(
        grants.chain(proposed.then_some(expected)),
        std::slice::from_ref(&expected.grantee),
        node,
        kind,
        &context,
    );
    if decision.answer == Answer::Deny && decision.grant().is_some() {
        return Err("workspace policy denies this action for the agent".to_owned());
    }
    Ok(decision.answer == Answer::Allow && (proposed || policy.grants.contains(expected)))
}

fn worded(text: &str) -> String {
    let mut kept = String::new();
    for character in text.chars().filter(|character| !character.is_control()) {
        if kept.len().saturating_add(character.len_utf8()) > MAX_WORDS_BYTES {
            break;
        }
        kept.push(character);
    }
    kept.trim().to_owned()
}

fn view(entry: &PolicyEntry) -> Permission {
    let id = entry.grant.id.as_str().to_owned();
    let mut arguments = Map::new();
    arguments.insert("permission".to_owned(), json!(id));
    Permission {
        id,
        source: Source::Asking,
        agent: AgentName {
            id: entry.scope.agent.clone(),
            name: entry.name.clone(),
        },
        workspace: entry.scope.workspace.clone(),
        action: entry.scope.action.clone(),
        on: entry.scope.on.clone(),
        words: entry.words.clone(),
        since_ms: entry.since_ms,
        asked_by: entry.asked_by.clone(),
        used_times: entry.used_times,
        last_used_ms: entry.last_used_ms,
        take_back: Call {
            provider: person_asking::PROVIDER.to_owned(),
            resource: person_asking::PERMISSIONS.to_owned(),
            action: person_asking::TAKE_BACK.to_owned(),
            arguments,
        },
    }
}

fn names_a_question(by: &AskedBy) -> bool {
    [&by.workspace, &by.conversation, &by.message]
        .into_iter()
        .all(|name| {
            !name.is_empty() && name.len() <= MAX_NAME_BYTES && !name.chars().any(char::is_control)
        })
}

impl Workspaces {
    fn asking(&self) -> Arc<Held> {
        let mut holders = HOLDERS.lock_recover();
        holders.retain(|_, (holder, _)| holder.strong_count() > 0);
        let (holder, kept) = holders
            .entry(self.inner.dir.clone())
            .or_insert_with(|| (Arc::downgrade(&self.inner), Arc::default()));
        if !std::ptr::eq(holder.as_ptr(), Arc::as_ptr(&self.inner)) {
            *holder = Arc::downgrade(&self.inner);
            *kept = Arc::default();
        }
        let kept = Arc::clone(kept);
        drop(holders);
        kept
    }

    /// Ask `person` through the server's single pending-confirmation path
    /// ([`super::AsksPeople`]).
    ///
    /// # Errors
    /// No approving device is configured or answered, or the question lapsed.
    pub async fn ask_person_confirmation(
        &self,
        person: [u8; 32],
        question: PendingConfirmation,
    ) -> Result<ConfirmationAnswer, String> {
        let asker = self
            .inner
            .asks_people
            .read_recover()
            .clone()
            .ok_or_else(|| "no approving device is available to answer".to_owned())?;
        let answer = asker.ask(person, question.clone()).await?;
        if !answer.answers(&question) {
            return Err("the answer is not offered by this question".to_owned());
        }
        Ok(answer)
    }

    /// Persist a lasting `tilo-action-policy` [`Grant`] for `scope` into its
    /// workspace's governed policy.
    ///
    /// # Errors
    /// The scope is invalid or the workspace policy could not be updated.
    pub async fn grant_always_in_workspace(
        &self,
        person: &[u8; 32],
        scope: &Asked,
    ) -> Result<Grant, String> {
        if !scope.is_named() {
            return Err(NOT_NAMED.to_owned());
        }
        let grant = governed_permission(person, scope).ok_or(NOT_NAMED)?;
        let workspace = self
            .workspace(&scope.workspace)
            .await
            .map_err(|err| err.to_string())?;
        let mut workspace = workspace.lock().await;
        let at = tilo_clock::wall();
        let policy = workspace.peer().governance().governed_policy()?;
        if !approval_in_policy(&policy, &grant, true)? {
            return Err(
                "workspace policy does not allow a lasting approval for this action".to_owned(),
            );
        }
        self.change_approval(&mut workspace, at, ApprovalChange::Keep(&grant))
            .await?;
        drop(workspace);
        Ok(grant)
    }

    /// Write a scoped grant change after checking it against the locked workspace.
    pub(super) async fn change_approval(
        &self,
        workspace: &mut GovernedWorkspace,
        at: SystemTime,
        change: ApprovalChange<'_>,
    ) -> Result<(), String> {
        if !self.governs(workspace) {
            return Err("this server's person does not govern the workspace".to_owned());
        }
        let key = &self.inner.host.key;
        let mut candidate = workspace.peer().clone();
        let governance = candidate.governance_mut();
        let changed = match change {
            ApprovalChange::Keep(grant) => {
                governance.put_governed_grants(std::slice::from_ref(grant), key, at)?
            }
            ApprovalChange::Revoke(ids) => governance.remove_governed_grants(ids, key, at)?,
        };
        if changed {
            let state = workspace.collaboration().clone();
            workspace.commit(candidate, key, at, |_, _| Ok(state))?;
            self.settle(workspace).await?;
        }
        Ok(())
    }

    /// Whether workspace policy still holds the approval this person gave for this scope.
    ///
    /// # Errors
    /// Workspace policy cannot be read or explicitly refuses the action.
    pub async fn governed_permission_stands(
        &self,
        person: &[u8; 32],
        scope: &Asked,
    ) -> Result<bool, String> {
        let expected = governed_permission(person, scope).ok_or(NOT_NAMED)?;
        let policy = self
            .policy_of(&scope.workspace)
            .await
            .ok_or_else(|| "the workspace's policy is not here".to_owned())?;
        approval_in_policy(&policy, &expected, false)
    }

    fn with_grants<T>(&self, work: impl FnOnce(&mut BTreeMap<String, Vec<PolicyEntry>>) -> T) -> T {
        let held = self.asking();
        let mut grants = held.grants.lock_recover();
        let map = grants.get_or_insert_with(|| load_grants(&self.inner.dir));
        let done = work(map);
        drop(grants);
        done
    }

    /// Decide whether a workspace `tilo-action-policy` grant covers `asked`.
    #[must_use]
    pub fn stands(&self, person: &[u8; 32], asked: &Asked) -> Option<String> {
        if !asked.is_named() {
            return None;
        }
        self.with_grants(|grants| {
            let entries = grants.get(&asked.workspace)?;
            let matching: Vec<&Grant> = entries
                .iter()
                .filter(|entry| entry.person == *person)
                .map(|entry| &entry.grant)
                .collect();
            let node = policy_node(asked);
            let grantees = [Grantee::agent(&asked.agent)];
            let decision = decide(
                matching,
                &grantees,
                &node,
                ActionKind::Outside,
                &Context::at(tilo_clock::unix_now_ms()),
            );
            (decision.answer == Answer::Allow)
                .then(|| decision.grant().map(|grant| grant.id.as_str().to_owned()))
                .flatten()
        })
    }

    /// Count a use of the workspace grant `id`.
    pub fn permission_used(&self, person: &[u8; 32], id: &str) {
        let updated = self.with_grants(|grants| {
            for entries in grants.values_mut() {
                if let Some(entry) = entries
                    .iter_mut()
                    .find(|entry| entry.person == *person && entry.grant.id.as_str() == id)
                {
                    entry.used_times = entry.used_times.saturating_add(1);
                    entry.last_used_ms = Some(tilo_clock::unix_now_ms());
                    return Some(entry.clone());
                }
            }
            None
        });
        if let Some(entry) = updated
            && let Err(error) = write_grant(&self.inner.dir, &entry)
        {
            tracing::warn!(%error, "a workspace grant's use was not persisted");
        }
    }

    /// Keep a workspace-scoped `tilo-action-policy` grant for `allowing`.
    ///
    /// # Errors
    /// The scope is not made of names, too many grants stand already, or it
    /// could not be persisted to the workspace store.
    pub fn let_always(&self, person: &[u8; 32], allowing: Allowing) -> Result<Permission, String> {
        let Allowing {
            scope,
            name,
            words,
            asked_by,
        } = allowing;
        if !scope.is_named() {
            return Err(NOT_NAMED.to_owned());
        }
        let id = id_of(&scope).ok_or(NOT_NAMED)?;
        self.with_grants(|grants| {
            let count: usize = grants
                .values()
                .flat_map(|entries| entries.iter())
                .filter(|entry| entry.person == *person)
                .count();
            let entries = grants.entry(scope.workspace.clone()).or_default();
            if let Some(existing) = entries
                .iter()
                .find(|entry| entry.person == *person && entry.grant.id.as_str() == id)
            {
                return Ok(view(existing));
            }
            if count >= MAX_PERMISSIONS {
                return Err(TOO_MANY.to_owned());
            }
            let grant = policy_grant(person, &id, &scope);
            let entry = PolicyEntry {
                person: *person,
                grant,
                scope,
                name: worded(&name),
                words: Words {
                    action: worded(&words.action),
                    on: worded(&words.on),
                    place: worded(&words.place),
                },
                since_ms: tilo_clock::unix_now_ms(),
                asked_by: asked_by.filter(names_a_question),
                used_times: 0,
                last_used_ms: None,
            };
            write_grant(&self.inner.dir, &entry)?;
            let shown = view(&entry);
            entries.push(entry);
            Ok(shown)
        })
    }

    /// Every workspace grant kept for `person`, oldest first.
    #[must_use]
    pub fn permissions_of(&self, person: &[u8; 32]) -> Permissions {
        let mut entries: Vec<PolicyEntry> = self.with_grants(|grants| {
            grants
                .values()
                .flat_map(|entries| entries.iter())
                .filter(|entry| entry.person == *person)
                .cloned()
                .collect()
        });
        entries.sort_by(|a, b| {
            a.since_ms
                .cmp(&b.since_ms)
                .then_with(|| a.grant.id.as_str().cmp(b.grant.id.as_str()))
        });
        Permissions {
            permissions: entries.iter().map(view).collect(),
        }
    }

    /// Revoke the grant `id` for `person`, and the approval the workspace's policy keeps for
    /// what it names: what that person let stand covers nothing from now on.
    ///
    /// # Errors
    /// The grant file could not be removed from the workspace store, or the workspace policy
    /// could not take the approval back.
    pub async fn take_back_permission(&self, person: &[u8; 32], id: &str) -> Result<bool, String> {
        let found = self.with_grants(|grants| {
            grants.values().find_map(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.person == *person && entry.grant.id.as_str() == id)
                    .cloned()
            })
        });
        let Some(entry) = found else {
            return Ok(false);
        };
        // The same allowance is a grant in the workspace's policy, under the approval name the
        // server writes when it is answered: it is taken back there first, or it still stands
        // while the person is told nothing does.
        if let Some(approval) = governed_permission(person, &entry.scope) {
            let workspace = self.workspace(&entry.scope.workspace).await;
            if let Ok(workspace) = workspace {
                let mut workspace = workspace.lock().await;
                if self.governs(&workspace) {
                    let at = tilo_clock::wall();
                    self.change_approval(
                        &mut workspace,
                        at,
                        ApprovalChange::Revoke(&[approval.id]),
                    )
                    .await?;
                }
            }
        }
        let ws = entry.scope.workspace.clone();
        self.with_grants(|grants| {
            for entries in grants.values_mut() {
                entries.retain(|other| !(other.person == *person && other.grant.id.as_str() == id));
            }
            grants.retain(|_, entries| !entries.is_empty());
        });
        if is_grant_id(id) {
            match std::fs::remove_file(grant_path(&self.inner.dir, &ws, id)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(true)
    }

    /// Note the questions waiting in `workspace`.
    pub fn note_waiting(&self, workspace: &str, waiting: Vec<(String, Waiting)>) {
        let kept = self.asking();
        let mut index = kept.waiting.0.lock_recover();
        if waiting.is_empty() {
            index.remove(workspace);
        } else {
            index.insert(workspace.to_owned(), waiting);
        }
        drop(index);
    }

    /// Every question that waits for `person`, oldest first.
    #[must_use]
    pub fn waiting_for(&self, person: &[u8; 32]) -> WaitingQuestions {
        let person = hex::encode(person);
        let kept = self.asking();
        let index = kept.waiting.0.lock_recover();
        let mut questions: Vec<Waiting> = index
            .values()
            .flatten()
            .filter(|(asked, _)| *asked == person)
            .map(|(_, waiting)| waiting.clone())
            .collect();
        drop(index);
        questions.sort_by(|a, b| {
            a.asked_at_ms
                .cmp(&b.asked_at_ms)
                .then_with(|| a.message.cmp(&b.message))
        });
        WaitingQuestions { questions }
    }

    async fn person_asking_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let person = Self::asking_person(caller, certificate, workspace_id)?;
        let address = &request.target.address;
        match (address.resource.as_str(), address.action.as_str()) {
            (person_asking::WAITING, person_asking::LIST) => {
                answer(&Ok::<_, String>(self.waiting_for(&person)))
            }
            (person_asking::PERMISSIONS, person_asking::LIST) => {
                answer(&Ok::<_, String>(self.permissions_of(&person)))
            }
            (person_asking::PERMISSIONS, person_asking::TAKE_BACK) => {
                let taking: TakeBack = argument(request, "the permission to take back")?;
                let left = match self.take_back_permission(&person, &taking.permission).await {
                    Ok(true) => Ok(self.permissions_of(&person)),
                    Ok(false) => Err(NONE_STANDS.to_owned()),
                    Err(reason) => Err(reason),
                };
                answer(&left)
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    pub(super) async fn person_alone_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        if request.target.provider == person_asking::PROVIDER {
            self.person_asking_invoke(workspace_id, caller, certificate, request)
                .await
        } else {
            self.person_vault_invoke(workspace_id, caller, certificate, request)
                .await
        }
    }
}
