//! What the device's record reader asks of the apps this device serves.
//!
//! The reader decides reads of a workspace's invocations, and whom an app's caller acts as, by
//! asking the app host ([`AppInvocations`]): invocations are not records of the workspace's store
//! yet, so only the host that dispatched them can answer for them. Once they are store records,
//! this trait goes and those reads go to the store like any other.

use tilo_actions::remote::workspace_invocations::{Invocation, InvocationBy, InvocationStatus};
use tilo_actions::remote::{ActionCallError, Caller, ResourceAddress, ResourceCopy, ResourceWake};
use tilo_broker_core::actor::BrokerActor;
use tilo_desktop_model::Workspace;
use tilo_remote_actions::asked::Waiting;

/// A store read's answer, once the app host has it ([`AppInvocations::store_read_record`]).
pub type StoreReading<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<serde_json::Value, ActionCallError>> + Send + 'a>,
>;

/// The app host's answers to the reads of a workspace's invocations, which it alone keeps
/// (see the module's documentation for why this exists).
pub trait AppInvocations: Send + Sync {
    /// The workspace's invocations as a record read, whole (an empty `record`) or one by its id,
    /// as `caller` may read each call there, with the calls in `asked` that wait for their
    /// person. What a program, an agent and a view read through the record API, never as an
    /// action.
    ///
    /// # Errors
    /// The caller may not read them.
    fn read_invocations(
        &self,
        caller: &Caller,
        workspace: &str,
        record: &str,
        asked: Vec<Waiting>,
    ) -> Result<ResourceCopy, ActionCallError>;

    /// Wake `wake` once the workspace's invocations next change, decided as a read is: the
    /// record of a call recorded there, or of one that has not ended, moving.
    ///
    /// # Errors
    /// The caller may not read them.
    fn watch_invocations(
        &self,
        caller: &Caller,
        workspace: &str,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError>;

    /// Who `caller`, a caller inside this device, reads `workspace`'s records as: the caller as
    /// the read is decided in that workspace, for the person it names.
    ///
    /// # Errors
    /// A caller that reads no records here.
    fn reading_caller(&self, caller: &Caller, workspace: &str) -> Result<Caller, ActionCallError>;

    /// The one actor the dispatch decides `caller`'s calls as, bounded by what its app declares.
    fn caller_actor(&self, caller: &Caller) -> BrokerActor;

    /// A store read of `address` for `caller` inside this device, the read decided already by
    /// the workspace the address names: it reads, and writes nothing.
    fn store_read_record<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
        read: serde_json::Value,
    ) -> StoreReading<'a>;

    /// The actions of `device`, a device of `workspace`, the way a call to an app's connection
    /// reaches it. `None` where this device cannot reach it.
    fn reach(
        &self,
        workspace: &str,
        device: [u8; 32],
    ) -> Option<tilo_peer_state::actions::RemoteDevice>;
}

/// What holds the app host without keeping it alive.
pub type WeakAppInvocations = std::sync::Weak<dyn AppInvocations>;

/// Whether `caller` may act in `workspace`. A caller confined to a workspace
/// was admitted to it by its connection; any other caller must be a
/// member.
pub fn may_act_in(caller: &Caller, workspace: &Workspace, now: u64) -> bool {
    // A placed device's caller takes part in no workspace's actions: it reads its records
    // where records are read, and nothing here.
    if caller.placed_records().is_some() {
        return false;
    }
    match caller.workspace() {
        // Confinement says which workspace a connection was admitted to, not
        // that an agent still belongs to it: an agent whose membership
        // was removed must lose its catalogue and authority at once. It
        // takes part as the unconfined branch decides: an enrolled agent
        // through its own grant, an agent a person's device handed this
        // session to (`Caller::delegated_to`) through that person's.
        Some(confined) => {
            confined == workspace.id().to_string()
                && if caller.acting().is_agent() {
                    BrokerActor::for_caller(caller).reads_in(workspace, now)
                } else {
                    true
                }
        }
        None => {
            // An application or a bot is always admitted into one workspace;
            // unconfined, it takes part in none.
            if caller.acting().app().is_some() || caller.acting().is_bot() {
                false
            } else {
                // Anyone else takes part in a workspace they may read; a foreign
                // device reads wherever its person does.
                BrokerActor::for_caller(caller).reads_in(workspace, now)
            }
        }
    }
}

/// The workspace's invocations as a dispatch that keeps no record of its own reads them: the calls
/// in `waiting` that wait for their person, whole or `record` alone.
///
/// # Errors
/// A call that does not read as JSON.
pub fn questions_reading(
    waiting: Vec<Waiting>,
    record: &str,
) -> Result<ResourceCopy, ActionCallError> {
    let mut items = Vec::new();
    with_questions(&mut items, waiting, (!record.is_empty()).then_some(record));
    if record.is_empty() {
        let waiting = items.len();
        return Ok(ResourceCopy::reading(
            serde_json::json!({ "items": items, "total": waiting, "waiting": waiting }),
            None,
        ));
    }
    Ok(match items.into_iter().next() {
        Some(invocation) => ResourceCopy::reading(
            serde_json::to_value(invocation).map_err(|error| ActionCallError::Unavailable {
                reason: error.to_string(),
            })?,
            None,
        ),
        None => ResourceCopy::Absent,
    })
}

/// `items`, a reading of a workspace's invocations, with what each call that waits in
/// `waiting` asks its person, and each such call the device keeps no record of (it waited across
/// a restart, or another dispatch here keeps it) listed beside them: only `only`, when the
/// reading names one.
pub fn with_questions(items: &mut Vec<Invocation>, waiting: Vec<Waiting>, only: Option<&str>) {
    use tilo_actions::remote::asked::AskedBy;
    for question in waiting {
        let id = question.invocation.0.to_string();
        if only.is_some_and(|only| only != id) {
            continue;
        }
        if let Some(item) = items
            .iter_mut()
            .find(|item| item.id.as_deref() == Some(id.as_str()))
        {
            item.status = InvocationStatus::Waiting;
            item.person = Some(question.person);
            item.asked = Some(Box::new(question.asked));
            continue;
        }
        let by = match &question.asked.facts.by {
            AskedBy::Agent(agent) => InvocationBy {
                name: agent.name.clone(),
                kind: "agent".to_owned(),
                id: Some(agent.id.clone()),
            },
            AskedBy::App { app, title, .. } => InvocationBy {
                name: title.clone(),
                kind: "app".to_owned(),
                id: Some(app.clone()),
            },
            AskedBy::Device(device) => InvocationBy {
                name: device.name.clone(),
                kind: "device".to_owned(),
                id: Some(device.id.clone()),
            },
            AskedBy::Program { name, app_id } => InvocationBy {
                name: name.clone(),
                kind: "program".to_owned(),
                id: app_id.clone(),
            },
        };
        let row = Invocation {
            id: Some(id.clone()),
            calls: vec![id],
            by,
            status: InvocationStatus::Waiting,
            started_ms: i64::try_from(question.asked.asked_at_ms).unwrap_or(i64::MAX),
            action: Some(question.asked.action.clone()),
            remembered: true,
            person: Some(question.person),
            asked: Some(Box::new(question.asked)),
            ..Invocation::default()
        };
        // Newest first, as the reading is.
        let at = items
            .iter()
            .position(|item| item.started_ms <= row.started_ms)
            .unwrap_or(items.len());
        items.insert(at, row);
    }
}
