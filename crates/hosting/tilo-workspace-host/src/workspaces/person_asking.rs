//! What stands for a person on this server: the actions their agents no
//! longer ask them about ([`tilo_actions::remote::person_asking`]).
//!
//! A lasting permission is kept in the person's own store here, beside
//! their vault and their model providers, one small file each: this server
//! decides an agent's action with every device of the person's off, so it
//! has to hold what they allowed, and nobody else has to. It is reached as
//! the vault is: the person themselves, on a device of theirs whose
//! certificate names them, reads the list and takes one back. Another
//! member reaches only their own, and an agent's request is refused,
//! whoever it acts for.
//!
//! Nothing a transport carries writes one. [`Workspaces::let_always`] is
//! called by the code this server works an agent's turn with, at the moment it
//! reads the person's own answer to a question it put itself, with the
//! scope it kept when it asked. [`Workspaces::stands`] is asked where an
//! action is decided, and reads the store each time: a permission taken
//! back is gone for the very next action.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_actions::remote::person_asking::{
    self, AgentName, Asked, AskedBy, Call, MAX_NAME_BYTES, MAX_WORDS_BYTES, Permission,
    Permissions, Source, TakeBack, Waiting, WaitingQuestions, Words,
};
use tilo_actions::remote::{ActionHostError, Caller, InvokeRequest};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

use super::person_settings::{answer, argument};
use super::{Workspaces, WorkspacesInner};

pub mod kept_asks;

/// The most permissions that stand for one person at a time.
pub const MAX_PERMISSIONS: usize = 256;

/// What [`Workspaces::let_always`] refuses a scope that is not made of names
/// with.
const NOT_NAMED: &str = "A lasting permission names its agent, its action and its workspace";
/// What it refuses one more than [`MAX_PERMISSIONS`] with.
const TOO_MANY: &str = "Too many permissions stand already: take one back first";
/// What taking back a permission that does not stand is refused with.
const NONE_STANDS: &str = "No such permission stands";

/// One writer at a time over what stands on this server: a use counted while
/// its permission is taken back must not write it again.
static WRITING: Mutex<()> = Mutex::new(());

/// Do `work` as the one writer.
fn writing<T>(work: impl FnOnce() -> T) -> T {
    let alone = WRITING.lock_recover();
    let done = work();
    drop(alone);
    done
}

/// The questions that wait for this server's people, by workspace: each with
/// the person it asks (their root key, hex). Whoever works the workspaces'
/// agents reads them from the conversations and says them here
/// ([`Workspaces::note_waiting`]); nothing is kept across a restart, the
/// conversations are.
#[derive(Default)]
struct WaitingIndex(Mutex<BTreeMap<String, Vec<(String, Waiting)>>>);

/// What a server keeps in memory of what its people are asked: the asks that
/// wait for an answer ([`kept_asks`]), and the questions that wait.
#[derive(Default)]
struct Held {
    asks: kept_asks::KeptAsks,
    waiting: WaitingIndex,
}

/// What each server of this process keeps in memory of what its people are
/// asked, by the directory its state is in, with the server it is of: a server
/// that starts again on the same state begins with nothing and reads its
/// disk. It is kept here and not with the server's other state, whose file
/// stands at its size ceiling.
type Holders = BTreeMap<PathBuf, (Weak<WorkspacesInner>, Arc<Held>)>;
static HOLDERS: Mutex<Holders> = Mutex::new(BTreeMap::new());

/// What a person's answer `always` lets stand.
#[derive(Clone, Debug)]
pub struct Allowing {
    /// What it covers, as the server kept it when it asked.
    pub scope: Asked,
    /// The name the person knows the agent under.
    pub name: String,
    /// The system's own words for the action, the thing and the workspace.
    pub words: Words,
    /// The question that was answered, where one was.
    pub asked_by: Option<AskedBy>,
}

/// One permission as this server keeps it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Kept {
    id: String,
    scope: Asked,
    name: String,
    words: Words,
    since_ms: u64,
    #[serde(default)]
    asked_by: Option<AskedBy>,
    #[serde(default)]
    used_times: u64,
    #[serde(default)]
    last_used_ms: Option<u64>,
}

/// Whether `id` is a permission's id: what names a file here.
fn is_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The id of the permission for `scope`: the same scope is the same
/// permission, so allowing it again adds none.
fn id_of(scope: &Asked) -> Option<String> {
    let named = serde_json::to_vec(scope).ok()?;
    Some(blake3::hash(&named).to_hex().chars().take(32).collect())
}

fn path_of(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

/// `text` as a permission is listed with it: on one line and short.
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

/// The permission kept at `path` under the id `id`. One that does not read,
/// or that is not the permission its file is named for, stands for nothing.
fn read(path: &Path, id: &str) -> Option<Kept> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "a lasting permission could not be read");
            return None;
        }
    };
    match serde_json::from_slice::<Kept>(&bytes) {
        Ok(kept) if kept.id == id && id_of(&kept.scope).as_deref() == Some(id) => Some(kept),
        Ok(_) => {
            tracing::warn!(path = %path.display(), "a lasting permission is not the one its file names");
            None
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "a lasting permission does not read");
            None
        }
    }
}

fn write(dir: &Path, kept: &Kept) -> Result<(), String> {
    let bytes = serde_json::to_vec(kept).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &path_of(dir, &kept.id),
        &bytes,
        0o600,
    )
    .map_err(|error| error.to_string())
}

/// Every permission kept in `dir`, oldest first.
fn all(dir: &Path) -> Vec<Kept> {
    let mut kept: Vec<Kept> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|file| {
            let path = file.path();
            let id = path.file_stem()?.to_str()?.to_owned();
            (is_id(&id)
                && path
                    .extension()
                    .is_some_and(|extension| extension == "json"))
            .then(|| read(&path, &id))
            .flatten()
        })
        .collect();
    kept.sort_by(|a, b| a.since_ms.cmp(&b.since_ms).then_with(|| a.id.cmp(&b.id)));
    kept
}

/// `kept` as the person reads it.
fn view(kept: Kept) -> Permission {
    let mut arguments = Map::new();
    arguments.insert("permission".to_owned(), json!(kept.id));
    Permission {
        id: kept.id,
        source: Source::Asking,
        agent: AgentName {
            id: kept.scope.agent,
            name: kept.name,
        },
        workspace: kept.scope.workspace,
        action: kept.scope.action,
        on: kept.scope.on,
        words: kept.words,
        since_ms: kept.since_ms,
        asked_by: kept.asked_by,
        used_times: kept.used_times,
        last_used_ms: kept.last_used_ms,
        take_back: Call {
            provider: person_asking::PROVIDER.to_owned(),
            resource: person_asking::PERMISSIONS.to_owned(),
            action: person_asking::TAKE_BACK.to_owned(),
            arguments,
        },
    }
}

/// Whether `by` names a question: three short names on one line.
fn names_a_question(by: &AskedBy) -> bool {
    [&by.workspace, &by.conversation, &by.message]
        .into_iter()
        .all(|name| {
            !name.is_empty() && name.len() <= MAX_NAME_BYTES && !name.chars().any(char::is_control)
        })
}

impl Workspaces {
    /// What this server keeps in memory of what its people are asked.
    fn asking(&self) -> Arc<Held> {
        let mut holders = HOLDERS.lock_recover();
        // A server that ended keeps nothing.
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

    /// Where this server keeps what stands for `person`.
    fn permissions_dir(&self, person: &[u8; 32]) -> PathBuf {
        self.person_settings_root(person)
            .join("asking")
            .join("permissions")
    }

    /// The permission of `person`'s that covers `asked`, by its id: one for
    /// exactly that thing, or one for every thing of the action's kind.
    /// `None` asks first. Read from the store at every call, so a permission
    /// taken back covers nothing from that moment.
    ///
    /// Reached from no transport: asked by the code that decides an action
    /// of an agent this server has for that person.
    #[must_use]
    pub fn stands(&self, person: &[u8; 32], asked: &Asked) -> Option<String> {
        if !asked.is_named() {
            return None;
        }
        let dir = self.permissions_dir(person);
        let wider = asked.on.is_some().then(|| asked.on_any());
        [Some(asked.clone()), wider]
            .into_iter()
            .flatten()
            .find_map(|scope| {
                let id = id_of(&scope)?;
                let kept = read(&path_of(&dir, &id), &id)?;
                (kept.scope == scope && kept.scope.covers(asked)).then_some(id)
            })
    }

    /// The action the permission `id` of `person`'s covers ran unasked:
    /// count it. A permission taken back meanwhile is not written again.
    pub fn permission_used(&self, person: &[u8; 32], id: &str) {
        if !is_id(id) {
            return;
        }
        let dir = self.permissions_dir(person);
        writing(|| {
            let Some(mut kept) = read(&path_of(&dir, id), id) else {
                return;
            };
            kept.used_times = kept.used_times.saturating_add(1);
            kept.last_used_ms = Some(tilo_clock::unix_now_ms());
            if let Err(error) = write(&dir, &kept) {
                tracing::warn!(%error, "a lasting permission's use was not counted");
            }
        });
    }

    /// Let what `allowing` names stand for `person` from now on. One that
    /// stands already stays as it is, since when it first stood.
    ///
    /// Reached from no transport, and no action leads here: the caller is
    /// the code that read the person's own answer, on a device of theirs,
    /// to a question this server put for an ask it kept, and `allowing` is
    /// what it kept then.
    ///
    /// # Errors
    /// The scope is not made of names, too many stand already, or it could
    /// not be written down.
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
        let dir = self.permissions_dir(person);
        writing(move || {
            if let Some(kept) = read(&path_of(&dir, &id), &id).filter(|kept| kept.scope == scope) {
                return Ok(view(kept));
            }
            if all(&dir).len() >= MAX_PERMISSIONS {
                return Err(TOO_MANY.to_owned());
            }
            let kept = Kept {
                id,
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
            write(&dir, &kept)?;
            Ok(view(kept))
        })
    }

    /// Everything that stands for `person`, oldest first.
    #[must_use]
    pub fn permissions_of(&self, person: &[u8; 32]) -> Permissions {
        Permissions {
            permissions: all(&self.permissions_dir(person))
                .into_iter()
                .map(view)
                .collect(),
        }
    }

    /// Take the permission `id` of `person`'s back: whether one stood. Its
    /// action asks again from this moment.
    ///
    /// # Errors
    /// It stands and could not be removed.
    pub fn take_back_permission(&self, person: &[u8; 32], id: &str) -> Result<bool, String> {
        if !is_id(id) {
            return Ok(false);
        }
        let path = path_of(&self.permissions_dir(person), id);
        writing(|| match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.to_string()),
        })
    }

    /// The questions that wait in `workspace` now, each with the person it
    /// asks (their root key, hex): what whoever works its agents read from
    /// its conversations. It replaces what was said of that workspace before.
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

    /// Every question that waits for `person`, over the workspaces this
    /// server keeps, oldest first.
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

    /// Answer a [`person_asking::PROVIDER`] request.
    fn person_asking_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
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
                let left = self
                    .take_back_permission(&person, &taking.permission)
                    .and_then(|stood| {
                        if stood {
                            Ok(self.permissions_of(&person))
                        } else {
                            Err(NONE_STANDS.to_owned())
                        }
                    });
                answer(&left)
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }

    /// Answer a request for what a person alone reaches of their own store
    /// on this server: their vault, or what their agents no longer ask them.
    pub(super) fn person_alone_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        if request.target.provider == person_asking::PROVIDER {
            self.person_asking_invoke(workspace_id, caller, certificate, request)
        } else {
            self.person_vault_invoke(workspace_id, caller, certificate, request)
        }
    }
}
