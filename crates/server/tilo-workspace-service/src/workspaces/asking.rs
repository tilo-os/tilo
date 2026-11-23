//! What waits for a person's yes on this host.
//!
//! An action the caller's grants leave to a person is refused with what they would be shown
//! ([`ActionCallError::ConfirmationRequired`]) unless the call carries their approve
//! ([`InvokeRequest::approval`]). Whoever dispatches the call keeps it as its own asked
//! `tilo.invocations` record ([`AskedRecords`]); nothing here keeps a question, waits for an
//! answer or asks a device. [`asked::WAITING`]`#`[`asked::LIST`] lists the asked records a
//! person may answer, across the workspaces here, read from those records as they sync.
//!
//! Each record written here is told to the person it waits for as a thing of their log
//! ([`Workspaces::tell`]), which their devices raise as a notification and which reaches a
//! browser of theirs when no device is looking: a question with Approve and Decline, which
//! answer the record itself, or, where only they can type what it takes, none, and opening it
//! shows the ask. Once the record is answered anywhere, the thing waits no more.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, TreePath, decide};
use tilo_actions::remote::asked::{
    self, AskedBy, AskedFacts, AskedList, AskedRecords, Named, RecordRef,
};
use tilo_actions::remote::person_telling::{About, Kind, Offered, OfferedTarget, Tell, Teller};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest, Pending};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

use super::person_settings::answer;
use super::{ToTell, WeakWorkspaces, Workspaces};

/// Who asks, as the system names `caller`: its agent by the name its key goes by, else the
/// person's own device.
#[must_use]
pub(crate) fn asked_by(caller: &Caller) -> AskedBy {
    match caller.acting().agent_id() {
        Some(agent) => AskedBy::Agent(Named {
            id: agent.to_owned(),
            name: caller.agent_called().unwrap_or(agent).to_owned(),
        }),
        None => AskedBy::default(),
    }
}

/// Go on where the person approved `request`; else it waits for their yes, showing them
/// `facts`.
///
/// # Errors
/// [`ActionCallError::ConfirmationRequired`] with `facts`, for a call nobody approved.
pub(crate) fn approved_or_asks(
    request: &InvokeRequest,
    facts: impl FnOnce() -> AskedFacts,
) -> Result<(), ActionCallError> {
    if request.approval().is_some() {
        Ok(())
    } else {
        Err(ActionCallError::asks(facts()))
    }
}

/// The provider of the `tilo.invocations` records, whose `resolve` answers one.
const INVOCATIONS: &str = "tilo.invocations";
/// The action that answers an asked record.
const RESOLVE: &str = "resolve";

/// The person `caller` asks for, as the request names them: the person an agent or app acts
/// for, else the person calling. Never whoever admitted the connection the call came on.
fn asked_person(caller: &Caller) -> Option<[u8; 32]> {
    let person = caller
        .acting_for_person()
        .or_else(|| caller.acting().person_id())?;
    hex::decode(person).ok()?.try_into().ok()
}

/// The id the record `record` is told under in its person's log.
fn told_id(record: &RecordRef) -> String {
    format!("asked:{}/{}", record.workspace, record.record)
}

/// What the person reads of `facts` where the record is told: the asker's own words, else
/// the verb and what it is on.
fn told_words(facts: &AskedFacts) -> String {
    if !facts.words.trim().is_empty() {
        return facts.words.clone();
    }
    match &facts.on {
        Some(on) => format!("{} {on}", facts.verb),
        None => facts.verb.clone(),
    }
}

/// The thing the asked `record` is told as: who asks, in their words, with Approve and
/// Decline answering the record, except where only the person can type what it takes.
fn telling(record: &RecordRef, facts: &AskedFacts) -> ToTell {
    let answer = |id: &str, label: &str, main: bool| {
        let mut arguments = serde_json::Map::new();
        arguments.insert("resolution".into(), Value::String(id.to_owned()));
        Offered {
            id: id.to_owned(),
            label: label.to_owned(),
            main,
            target: OfferedTarget {
                app: INVOCATIONS.to_owned(),
                record: String::new(),
                resource: record.record.clone(),
                action: RESOLVE.to_owned(),
            },
            arguments,
        }
    };
    let actions = if facts.takes.is_some() {
        Vec::new()
    } else {
        vec![
            answer("approve", "Approve", true),
            answer("decline", "Decline", false),
        ]
    };
    let from = match &facts.by {
        AskedBy::Agent(agent) => Some(Teller {
            agent: agent.id.clone(),
            name: agent.name.clone(),
        }),
        AskedBy::App { .. } | AskedBy::Device(_) => None,
    };
    ToTell {
        id: Some(told_id(record)),
        kind: Kind::Question,
        tell: Tell {
            words: told_words(facts),
            about: Some(About {
                workspace: record.workspace.clone(),
                app: INVOCATIONS.to_owned(),
                record: String::new(),
                resource: record.record.clone(),
            }),
            actions,
            ..Tell::default()
        },
        from,
        workspace: Some(record.workspace.clone()),
        words_stay: false,
        spender: None,
    }
}

/// The asked records as the host keeps them: each one written is told to the person it waits
/// for, and taken back from their log once it is answered.
struct Told {
    records: Arc<dyn AskedRecords>,
    workspaces: WeakWorkspaces,
    /// The records told and not yet seen answered, with the person each waits for.
    waiting: Mutex<BTreeSet<([u8; 32], RecordRef)>>,
}

impl Told {
    /// Take back from their log each record told that waits no more.
    fn settle(&self, workspaces: &Workspaces) {
        let waiting = self.waiting.lock_recover().clone();
        let mut asked: std::collections::BTreeMap<[u8; 32], BTreeSet<RecordRef>> =
            std::collections::BTreeMap::new();
        for (person, record) in waiting {
            let still = asked.entry(person).or_insert_with(|| {
                self.records
                    .asked_of(&person)
                    .into_iter()
                    .map(|asked| asked.record)
                    .collect()
            });
            if still.contains(&record) {
                continue;
            }
            if let Err(error) = workspaces.withdraw_told(&person, &told_id(&record)) {
                tracing::debug!(%error, "an answered ask stays in its person's log");
            }
            self.waiting.lock_recover().remove(&(person, record));
        }
    }
}

impl AskedRecords for Told {
    fn write_asked(
        &self,
        workspace: &str,
        caller: &Caller,
        request: &InvokeRequest,
        facts: &AskedFacts,
    ) -> Result<RecordRef, String> {
        let record = self
            .records
            .write_asked(workspace, caller, request, facts)?;
        if let (Some(person), Some(workspaces)) = (asked_person(caller), self.workspaces.upgrade())
        {
            self.waiting.lock_recover().insert((person, record.clone()));
            let told = telling(&record, facts);
            tokio::spawn(async move {
                if let Err(error) = workspaces.tell(&person, told).await {
                    tracing::warn!(%error, "an asked record was not told to its person");
                }
            });
        }
        Ok(record)
    }

    fn asked_of(&self, person: &[u8; 32]) -> Vec<asked::Asked> {
        self.records.asked_of(person)
    }

    fn changed(&self) -> Pending<()> {
        self.records.changed()
    }
}

impl Workspaces {
    /// Keep every call that waits for a person's yes as its asked record through `records`:
    /// the workspaces' store, which the host hands this once. Each record written is told to
    /// its person. What the host's other dispatches keep their asked calls through is
    /// [`Self::asked_records`], the same store, telling the same way.
    pub fn keep_asked_in(&self, records: Arc<dyn AskedRecords>) {
        let told = Arc::new(Told {
            records,
            workspaces: self.downgrade(),
            waiting: Mutex::default(),
        });
        if self.inner.asked_records.set(told.clone()).is_err() {
            return;
        }
        let follows = Arc::downgrade(&told);
        tokio::spawn(async move {
            loop {
                let Some(changed) = follows.upgrade().map(|told| told.records.changed()) else {
                    return;
                };
                if changed.await.is_none() {
                    return;
                }
                let (Some(told), Some(workspaces)) = (follows.upgrade(), told_workspaces(&follows))
                else {
                    return;
                };
                told.settle(&workspaces);
            }
        });
    }

    /// Where calls that wait for a person's yes are kept, once the host named it: what every
    /// dispatch of this host keeps its asked calls through.
    pub fn asked_records(&self) -> Option<Arc<dyn AskedRecords>> {
        self.inner.asked_records.get().cloned()
    }

    /// Keep `request`, which `caller` made and which waits for its person's yes showing them
    /// `facts`, as its asked record: in the workspace the call acts in, else in the person's
    /// own workspace here. The person asked is the request's own, as its caller names them,
    /// never whoever admitted the connection it came on. `None` where no store here keeps
    /// asked records.
    pub(crate) async fn keep_asked(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        request: &InvokeRequest,
        facts: &AskedFacts,
    ) -> Option<Result<RecordRef, String>> {
        let records = self.asked_records()?;
        let workspace = match workspace_id {
            Some(id) => self.workspace_name(id).await,
            None => caller
                .user_public_key()
                .and_then(|person| self.own_workspace_of(person)),
        };
        Some(match workspace {
            Some(workspace) => records.write_asked(&workspace, caller, request, facts),
            None => Err("no workspace here keeps it for its person".to_owned()),
        })
    }

    /// Whether a grant in `workspace`'s policy lets `agent` take `node`, an action that
    /// reaches outside, without its person's yes each time: a grant decides it, never a
    /// default.
    pub async fn granted(&self, workspace: &str, agent: &str, node: &TreePath) -> bool {
        let Some(policy) = self.policy_of(workspace).await else {
            return false;
        };
        let decision = decide(
            policy.grants.iter(),
            &[Grantee::agent(agent)],
            node,
            ActionKind::Outside,
            &Context::at(tilo_clock::unix_now_ms()),
        );
        decision.answer == Answer::Allow && decision.grant().is_some()
    }

    /// Answer a [`asked::PROVIDER`] request: every asked record the calling person may answer.
    pub(super) fn asking_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let person = Self::asking_person(caller, certificate, workspace_id)?;
        let address = &request.target.address;
        if (address.resource.as_str(), address.action.as_str()) != (asked::WAITING, asked::LIST) {
            return Err(ActionCallError::UnknownAction);
        }
        let questions = self
            .asked_records()
            .map(|records| records.asked_of(&person))
            .unwrap_or_default();
        answer(&Ok::<_, String>(AskedList { questions }))
    }
}

/// The workspaces `told` tells for, while both are still there.
fn told_workspaces(told: &std::sync::Weak<Told>) -> Option<Workspaces> {
    told.upgrade()?.workspaces.upgrade()
}
