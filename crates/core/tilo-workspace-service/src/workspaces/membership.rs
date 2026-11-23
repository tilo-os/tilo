//! What a workspace keeps of who is in it, as records of the workspace itself: each member's
//! membership ([`Membership`]: the role they hold and when the workspace was made), which its
//! members read as the workspace's grants allow, and what its owner alone keeps of it
//! ([`Ownership`]: what they labelled it with, and whether keys reaching it alone run commands in
//! its machine unasked), which no role but the owner's reads or changes
//! ([`tilo_action_policy::roles::OWNER_ONLY_APP`]). A grant is decided per record, so the two
//! are two records: a reader filters no field.
//!
//! A host that holds the workspace keeps both, whether or not it holds its owner's own
//! workspace. Every listing of a person's workspaces joins each entry with them.

use std::collections::BTreeMap;

use tilo_resource::{Model, Refusal, resource};

/// The app whose records the members' memberships are.
pub const APP: &str = "so.tilo.membership";

/// The record of what the workspace's owner alone keeps of it.
pub const OWNERSHIP: (&str, &str) = (tilo_action_policy::roles::OWNER_ONLY_APP, "ownership");

/// The most labels one workspace carries.
const MAX_LABELS: usize = 64;
/// The longest name or value of a label, bytes.
const MAX_LABEL: usize = 256;

/// The record of `person`'s membership of the workspace.
#[must_use]
pub fn member_record(person: &[u8; 32]) -> String {
    format!("member.{}", hex::encode(person))
}

/// One member's membership of the workspace.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Membership {
    /// The role they hold there.
    pub role: Option<String>,
    /// When the workspace was made, ms since the epoch.
    pub made_at_ms: Option<i64>,
}

#[resource(kind = "tilo.membership", read_content = tilo_resource::read::fields)]
impl Membership {
    /// Keep that the member holds `role`, in a workspace made at `made_at_ms`.
    #[action(housekeeping)]
    pub fn keep_role(&mut self, role: String, made_at_ms: i64) -> Result<(), Refusal> {
        if role.trim().is_empty() {
            return Err(Refusal::new("a membership names its role"));
        }
        self.role = Some(role);
        if self.made_at_ms.is_none() {
            self.made_at_ms = Some(made_at_ms);
        }
        Ok(())
    }
}

/// What the workspace's owner alone keeps of it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Ownership {
    /// What they labelled it with (`customer_id` → `cus_8121`), for their programs to find it by.
    pub labels: BTreeMap<String, String>,
    /// Keys reaching this workspace alone run commands in its machine without asking them each
    /// time. Off until they turn it on.
    pub unattended: bool,
}

#[resource(kind = "tilo.ownership", read_content = tilo_resource::read::fields)]
impl Ownership {
    /// Label the workspace with `labels`, a JSON object of names to values, in place of what it
    /// was labelled with before: an empty one takes them all off.
    #[action]
    pub fn relabel(&mut self, labels: String) -> Result<(), Refusal> {
        let labels: BTreeMap<String, String> = serde_json::from_str(&labels)
            .map_err(|_| Refusal::new("labels are names with a value each"))?;
        if labels.len() > MAX_LABELS
            || labels.iter().any(|(name, value)| {
                name.is_empty() || name.len() > MAX_LABEL || value.len() > MAX_LABEL
            })
        {
            return Err(Refusal::new(format!(
                "a workspace carries at most {MAX_LABELS} labels, each named in 1 to {MAX_LABEL} \
                 bytes with a value of at most {MAX_LABEL}"
            )));
        }
        self.labels = labels;
        Ok(())
    }

    /// Let keys reaching the workspace alone run commands in its machine unasked, or stop them.
    #[action]
    pub fn run_unattended(&mut self, on: bool) -> Result<(), Refusal> {
        self.unattended = on;
        Ok(())
    }
}
