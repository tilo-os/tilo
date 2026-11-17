//! How far a bot's schedules have been answered, kept in the workspace so
//! whichever participant runs the bot carries on from there.
//!
//! A bot is installed in the workspace, not on a host: its schedules are the
//! workspace's work, and which participant fires one is the lease's call
//! ([`super::leases`]). What must not be a host's own file is the progress:
//! each participant that fires a schedule records the fire time it answered
//! under its own device, and every participant reads the latest answer of
//! them. A missed fire time is derived from that answer
//! ([`tilo_automation::schedule::plan`]), so it is caught up once whichever
//! device takes over and however long no device ran, never once per period.
//!
//! Rows are per device, like lease claims: a device writes only its own row,
//! so two participants never overwrite each other and the answer after a merge
//! is the latest row. A program updated to another trigger starts its own
//! times: rows name the trigger they answered, and one that does not match the
//! installed trigger is not an answer to it.

use super::ROOT;
use super::SystemTime;
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use automerge::transaction::Transactable as _;
use automerge::{ObjType, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};

/// Key of the bots' answered fire times in the workspace's own document: a map
/// from each installation to a map from each trigger index to a map from each
/// participant device key (hex) to its answer.
pub const BOT_SCHEDULES_KEY: &str = "bot_schedules_v1";

/// What one device answered for one trigger: which trigger it answered, and
/// the last fire time it answered.
#[derive(Serialize, Deserialize)]
struct StoredAnswer {
    trigger: String,
    answered_ms: i64,
}

/// The mark of `trigger`, as a schedule's answer names the trigger it belongs
/// to: a program updated to another trigger marks differently and starts its
/// own times.
#[must_use]
pub fn trigger_mark(trigger: &tilo_automation::TriggerSpec) -> String {
    let bytes = serde_json::to_vec(trigger).unwrap_or_default();
    hex::encode(blake3::hash(&bytes).as_bytes())
}

/// The latest fire time any device answered for `trigger` of `installation`,
/// if any did.
fn answered_of(
    doc: Option<&automerge::AutoCommit>,
    installation: &str,
    trigger: usize,
    mark: &str,
) -> Option<i64> {
    let doc = doc?;
    let (Value::Object(ObjType::Map), installations) = doc.get(ROOT, BOT_SCHEDULES_KEY).ok()??
    else {
        return None;
    };
    let (Value::Object(ObjType::Map), triggers) = doc.get(&installations, installation).ok()??
    else {
        return None;
    };
    let (Value::Object(ObjType::Map), devices) =
        doc.get(&triggers, trigger.to_string().as_str()).ok()??
    else {
        return None;
    };
    doc.keys(&devices)
        .filter_map(|device| {
            let (Value::Scalar(value), _) = doc.get(&devices, device.as_str()).ok()?? else {
                return None;
            };
            let ScalarValue::Str(json) = value.as_ref() else {
                return None;
            };
            let answer: StoredAnswer = serde_json::from_str(json).ok()?;
            (answer.trigger == mark).then_some(answer.answered_ms)
        })
        .max()
}

impl PeerWorkspace<'_> {
    /// The latest fire time any participant answered for the schedule at
    /// `trigger` of `installation`'s program, `mark` being its
    /// [`trigger_mark`]; `None` when no participant has seen it before.
    #[must_use]
    pub fn bot_schedule_answered(
        self,
        installation: &str,
        trigger: usize,
        mark: &str,
    ) -> Option<i64> {
        answered_of(
            self.peer.core.workspace_doc.as_deref(),
            installation,
            trigger,
            mark,
        )
    }
}

impl PeerWorkspaceMut<'_> {
    /// Record that `device`, a member's device whose exact current
    /// certificate this workspace holds and who may change it, answered
    /// through `answered_ms` for the schedule at `trigger` of `installation`'s
    /// program, marked `mark`. A smaller answer than one already kept never
    /// replaces it. Answers whether the workspace changed.
    ///
    /// # Errors
    ///
    /// `device` is not allowed to change this workspace, or the answer cannot
    /// be written.
    pub fn answer_bot_schedule(
        &mut self,
        device: &tilo_user_identity::DeviceCert,
        installation: &str,
        trigger: usize,
        mark: &str,
        answered_ms: i64,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_writer(device, now)?;
        if self
            .peer
            .workspace()
            .bot_schedule_answered(installation, trigger, mark)
            .is_some_and(|answered| answered >= answered_ms)
        {
            return Ok(false);
        }
        let json = serde_json::to_string(&StoredAnswer {
            trigger: mark.to_owned(),
            answered_ms,
        })
        .map_err(|error| error.to_string())?;
        let actor = self.peer.workspace_actor_for(&device.device_public_key);
        let workspace = self
            .peer
            .core
            .workspace_doc
            .as_deref()
            .ok_or("schedule workspace is absent")?;
        let mut candidate = workspace.clone();
        candidate.set_actor(actor);
        let installations = match candidate.get(ROOT, BOT_SCHEDULES_KEY).map_err(message)? {
            Some((Value::Object(ObjType::Map), installations)) => installations,
            Some(_) => return Err("the workspace's bot schedules are malformed".into()),
            None => candidate
                .put_object(ROOT, BOT_SCHEDULES_KEY, ObjType::Map)
                .map_err(message)?,
        };
        let triggers = match candidate
            .get(&installations, installation)
            .map_err(message)?
        {
            Some((Value::Object(ObjType::Map), triggers)) => triggers,
            Some(_) => return Err(format!("bot `{installation}`'s schedules are malformed")),
            None => candidate
                .put_object(&installations, installation, ObjType::Map)
                .map_err(message)?,
        };
        let trigger = trigger.to_string();
        let devices = match candidate
            .get(&triggers, trigger.as_str())
            .map_err(message)?
        {
            Some((Value::Object(ObjType::Map), devices)) => devices,
            Some(_) => return Err(format!("schedule {trigger} is malformed")),
            None => candidate
                .put_object(&triggers, trigger.as_str(), ObjType::Map)
                .map_err(message)?,
        };
        candidate
            .put(
                &devices,
                hex::encode(device.device_public_key).as_str(),
                json.as_str(),
            )
            .map_err(message)?;
        candidate.commit();
        self.peer.core.workspace_doc = Some(candidate.into());
        Ok(true)
    }
}

/// A document operation's error as text.
fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}
