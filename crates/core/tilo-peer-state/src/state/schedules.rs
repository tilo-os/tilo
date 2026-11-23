//! How far an automation's schedules have been answered (a saved shortcut's, a
//! bot's), kept in the workspace so whichever device fires the next one
//! carries on from there.
//!
//! An automation is saved in the workspace, not on a device: its schedules are
//! the workspace's work, and which device fires one is the lease's call
//! ([`super::leases`]). What must not be a device's own file is the progress:
//! each key holder that fires a schedule records the fire time it answered
//! under its own device, and every key holder reads the latest answer of
//! them. A missed fire time is derived from that answer
//! ([`tilo_automation::schedule::plan`]), so it is caught up once whichever
//! device takes over and however long no device ran, never once per period.
//!
//! Rows are per device, like lease claims: a device writes only its own row,
//! so two key holders never overwrite each other and the answer after a merge
//! is the latest row. A program updated to another trigger starts its own
//! times: rows name the trigger they answered, and one that does not match the
//! installed trigger is not an answer to it.
//!
//! A trigger that is no schedule keeps its progress in the same rows
//! ([`PeerWorkspace::trigger_progress`]): what the looks at a feed or a page
//! took in, which moments of a calendar were answered. It is whatever its
//! trigger makes of it, kept whole by the device that last looked, and the
//! latest row is what the next device to look carries on from. A schedule's
//! answer and a trigger's progress never read as each other.

use super::SystemTime;
use super::device_rows;
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use serde::{Deserialize, Serialize};

/// Key of the automations' answered fire times in the workspace's own
/// document: a map, there from the workspace's founding, from
/// `<automation>/<trigger index>/<device key hex>` to its answer
/// ([`device_rows`]).
pub const SCHEDULES_KEY: &str = "bot_schedules_v1";

/// What one device answered for one trigger: which trigger it answered, and
/// the last fire time it answered.
#[derive(Serialize, Deserialize)]
struct StoredAnswer {
    trigger: String,
    answered_ms: i64,
}

/// What one device kept of a trigger's progress: which trigger it is the
/// progress of, when the device kept it, and the progress itself.
#[derive(Serialize, Deserialize)]
struct StoredProgress {
    trigger: String,
    kept_ms: i64,
    progress: serde_json::Value,
}

/// The mark of `trigger`, as a schedule's answer names the trigger it belongs
/// to: a program updated to another trigger marks differently and starts its
/// own times.
#[must_use]
pub fn trigger_mark(trigger: &tilo_automation::TriggerSpec) -> String {
    let bytes = serde_json::to_vec(trigger).unwrap_or_default();
    hex::encode(blake3::hash(&bytes).as_bytes())
}

/// The scope a device's answer for `trigger` of `automation` sits under.
fn scope(automation: &str, trigger: usize) -> String {
    format!("{automation}/{trigger}")
}

/// The latest fire time any device answered for `trigger` of `automation`,
/// if any did.
fn answered_of(
    doc: Option<&automerge::AutoCommit>,
    automation: &str,
    trigger: usize,
    mark: &str,
) -> Option<i64> {
    device_rows::rows_of(doc, SCHEDULES_KEY, &scope(automation, trigger))
        .into_iter()
        .filter_map(|(_, json)| {
            let answer: StoredAnswer = serde_json::from_str(&json).ok()?;
            (answer.trigger == mark).then_some(answer.answered_ms)
        })
        .max()
}

impl PeerWorkspace<'_> {
    /// The latest fire time any key holder answered for the schedule at
    /// `trigger` of `automation`'s program, `mark` being its
    /// [`trigger_mark`]; `None` when no key holder has seen it before.
    #[must_use]
    pub fn schedule_answered(self, automation: &str, trigger: usize, mark: &str) -> Option<i64> {
        answered_of(
            self.peer.core.workspace_doc.as_deref(),
            automation,
            trigger,
            mark,
        )
    }

    /// The progress the device that last kept one kept for `trigger` of
    /// `automation`, `mark` being its [`trigger_mark`]; `None` when no device
    /// kept one for that trigger.
    #[must_use]
    pub fn trigger_progress(
        self,
        automation: &str,
        trigger: usize,
        mark: &str,
    ) -> Option<serde_json::Value> {
        device_rows::rows_of(
            self.peer.core.workspace_doc.as_deref(),
            SCHEDULES_KEY,
            &scope(automation, trigger),
        )
        .into_iter()
        .filter_map(|(device, json)| {
            let kept: StoredProgress = serde_json::from_str(&json).ok()?;
            (kept.trigger == mark).then_some((kept.kept_ms, device, kept.progress))
        })
        .max_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)))
        .map(|(_, _, progress)| progress)
    }
}

impl PeerWorkspaceMut<'_> {
    /// Keep `progress` as what `device`, a member's device whose exact
    /// current certificate this workspace holds and who may change it, made
    /// of `trigger` of `automation`, marked `mark`, at `kept_ms`. Answers
    /// whether the workspace changed.
    ///
    /// # Errors
    ///
    /// `device` is not allowed to change this workspace, or the progress
    /// cannot be written.
    pub fn keep_trigger_progress(
        &mut self,
        device: &tilo_user_identity::DeviceCert,
        automation: &str,
        trigger: usize,
        mark: &str,
        (progress, kept_ms): (&serde_json::Value, i64),
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_writer(device, now)?;
        let json = serde_json::to_string(&StoredProgress {
            trigger: mark.to_owned(),
            kept_ms,
            progress: progress.clone(),
        })
        .map_err(|error| error.to_string())?;
        let key = device_rows::row_key(&scope(automation, trigger), &device.device_public_key);
        self.write_row(
            SCHEDULES_KEY,
            &key,
            &device.device_public_key,
            Some(&json),
            now,
        )?;
        Ok(true)
    }

    /// Record that `device`, a member's device whose exact current
    /// certificate this workspace holds and who may change it, answered
    /// through `answered_ms` for the schedule at `trigger` of `automation`'s
    /// program, marked `mark`. A smaller answer than one already kept never
    /// replaces it. Answers whether the workspace changed.
    ///
    /// # Errors
    ///
    /// `device` is not allowed to change this workspace, or the answer cannot
    /// be written.
    pub fn answer_schedule(
        &mut self,
        device: &tilo_user_identity::DeviceCert,
        automation: &str,
        trigger: usize,
        mark: &str,
        answered_ms: i64,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_writer(device, now)?;
        if self
            .peer
            .workspace()
            .schedule_answered(automation, trigger, mark)
            .is_some_and(|answered| answered >= answered_ms)
        {
            return Ok(false);
        }
        let json = serde_json::to_string(&StoredAnswer {
            trigger: mark.to_owned(),
            answered_ms,
        })
        .map_err(|error| error.to_string())?;
        let key = device_rows::row_key(&scope(automation, trigger), &device.device_public_key);
        self.write_row(
            SCHEDULES_KEY,
            &key,
            &device.device_public_key,
            Some(&json),
            now,
        )?;
        Ok(true)
    }
}
