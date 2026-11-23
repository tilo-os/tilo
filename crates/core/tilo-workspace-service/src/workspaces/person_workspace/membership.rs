//! A person's membership of one of their workspaces, as their own workspace keeps it: the role
//! they hold there, when it was made, what they labelled it with, and whether they let keys
//! reaching it alone run commands in its machine unasked.
//!
//! It is a record of the person's own workspace, one per workspace ([`record`]), so only the
//! person and the grants they give read it: nobody invited into the workspace reads their
//! labels, and no key working in the workspace turns running unattended on for itself. Every
//! listing of the person's workspaces joins each entry with it.

use std::collections::BTreeMap;

use tilo_resource::{Model, Refusal, resource};

/// The app whose records the memberships are, in the person's own workspace.
pub const APP: &str = "so.tilo.identity";

/// The most labels one workspace carries.
const MAX_LABELS: usize = 64;
/// The longest name or value of a label, bytes.
const MAX_LABEL: usize = 256;

/// The record of the person's membership of `workspace` (its UUID), in their own workspace.
#[must_use]
pub fn record(workspace: &str) -> String {
    format!("identity.membership.{workspace}")
}

/// The person's membership of one workspace.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Membership {
    /// The role they hold there.
    pub role: Option<String>,
    /// When the workspace was made, ms since the epoch.
    pub made_at_ms: Option<i64>,
    /// What they labelled it with (`customer_id` → `cus_8121`), for their programs to find it by.
    pub labels: BTreeMap<String, String>,
    /// Keys reaching this workspace alone run commands in its machine without asking them each
    /// time. Off until they turn it on.
    pub unattended: bool,
}

#[resource(kind = "tilo.membership", read_content = tilo_resource::read::fields)]
impl Membership {
    /// Keep that the person holds `role` in the workspace, made at `made_at_ms`.
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

    /// The workspace was deleted: nothing of the membership stays.
    #[action(housekeeping)]
    pub fn forget(&mut self) -> Result<(), Refusal> {
        self.role = None;
        self.made_at_ms = None;
        self.labels = BTreeMap::new();
        self.unattended = false;
        Ok(())
    }
}
