//! The policy a worker reads a workspace's records under.

/// Current governed read policy supplied by the authenticated host boundary.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ViewReadPolicy {
    /// Governed workspace whose current policy authorized the exact source record.
    pub workspace: String,
    /// The certified caller's current member access in the workspace, including the
    /// bound of a device that is not theirs.
    pub access: ui::permissions::MemberAccess,
    /// The workspace's current sandbox capabilities.
    pub sandbox: Vec<collab_types::caps::CapOverride>,
    /// Host time used to evaluate capability expiry.
    pub unix_time: u64,
    /// What the workspace's list of its agents bounds the agent asking to, when an agent asks
    /// for the person (the workspace's list of its agents): empty follows the default.
    #[serde(default)]
    pub agent_bound: Vec<ui::permissions::policy::Rule>,
}

impl ViewReadPolicy {
    pub(crate) fn permissions(&self) -> ui::permissions::PermissionContext {
        ui::permissions::PermissionContext::new(
            ui::permissions::Acting::person(self.access.person_id().unwrap_or_default()),
            self.sandbox.clone(),
            self.access.clone(),
        )
        .at_unix_time(self.unix_time)
    }
}
