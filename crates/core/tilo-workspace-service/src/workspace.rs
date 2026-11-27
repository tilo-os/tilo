//! Per-workspace routing and authorization metadata: what each member device
//! may do, and who the workspace is attached to. Who a workspace has in it, and
//! its changes, are not here: both are its signed governance's, which its links
//! read ([`crate::workspace_access`]).

use std::collections::HashMap;
use std::sync::Arc;

use crate::workspace_files::WorkspaceFiles;
use tilo_server_protocol::WorkspaceId;

/// The access of each member device of a workspace, by device key.
pub type DeviceAccess = std::collections::BTreeMap<[u8; 32], tilo_action_policy::MemberAccess>;

/// Per-workspace runtime.
pub struct WorkspaceRuntime {
    /// The workspace this runtime is of, by the one id a workspace has.
    workspace_id: WorkspaceId,
    /// Whether the workspace's host follows its app surfaces arriving.
    pub(crate) followed: bool,
    /// The in-process authority serving this workspace, if any: it answers
    /// the workspace's calls.
    pub workspace_authority: Option<crate::workspaces::WorkspaceAuthority>,
    /// The access of each member device, as the workspace's signed governance
    /// gives it: what the server decides each change's claimed action against.
    /// `None` until it settled here; the server then decides changes against
    /// their app surface alone.
    pub(crate) device_access: Option<Arc<DeviceAccess>>,
    /// The workspace's files on this host.
    pub files: WorkspaceFiles,
    /// The workspace left the host's registry ([`crate::workspace_io::evict`]):
    /// its files stay, and this runtime writes them no more.
    pub(crate) evicted: bool,
    /// What services beside the workspace's own stack keep for it (a
    /// server's compute routing), one value per type.
    pub extensions: Extensions,
}

impl WorkspaceRuntime {
    /// The workspace this runtime is of.
    #[must_use]
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// Take `device_access`, what each member device may do now, as what the
    /// workspace's changes are decided against: `None` where the workspace is
    /// served here no more. Answers whether a device had access before and
    /// has none now, so its links re-check on the announced change. One that
    /// only adds devices wakes nobody: a public workspace admitting a crowd
    /// would otherwise re-audit every member's link for every newcomer.
    pub(crate) fn settle_access(&mut self, device_access: Option<DeviceAccess>) -> bool {
        let left = match (&self.device_access, &device_access) {
            (Some(previous), Some(next)) => {
                previous.keys().any(|device| !next.contains_key(device))
            }
            (Some(previous), None) => !previous.is_empty(),
            (None, _) => false,
        };
        if let Some(device_access) = device_access {
            self.device_access = Some(Arc::new(device_access));
        }
        left
    }

    /// Build a runtime for a bound workspace from the files its host keeps.
    ///
    /// # Errors
    ///
    /// The workspace's directory can't be created, or its access record
    /// can't be read.
    pub fn open(
        state_root: &std::path::Path,
        workspace_id: WorkspaceId,
    ) -> Result<Self, crate::workspace_files::FilesError> {
        let files = WorkspaceFiles::open(state_root, workspace_id)?;
        Ok(Self {
            workspace_id,
            files,
            evicted: false,
            extensions: Extensions::default(),
            followed: false,
            workspace_authority: None,
            device_access: None,
        })
    }
}

/// Shorthand: a workspace runtime held under an async mutex.
pub type SharedWorkspace = Arc<tokio::sync::Mutex<WorkspaceRuntime>>;

/// What services beside the workspace's own stack keep per workspace, one value per type: a
/// server keeps its compute routing here. The workspace knows nothing of what they hold.
#[derive(Default)]
pub struct Extensions(HashMap<std::any::TypeId, Box<dyn std::any::Any + Send>>);

impl Extensions {
    /// The value of type `T`, when one was made.
    #[must_use]
    pub fn get<T: std::any::Any + Send>(&self) -> Option<&T> {
        self.0
            .get(&std::any::TypeId::of::<T>())
            .and_then(|value| value.downcast_ref())
    }

    /// The value of type `T`, made by `make` the first time.
    pub fn get_or_insert_with<T: std::any::Any + Send>(
        &mut self,
        make: impl FnOnce() -> T,
    ) -> &mut T {
        let value = self
            .0
            .entry(std::any::TypeId::of::<T>())
            .or_insert_with(|| Box::new(make()));
        #[expect(
            clippy::expect_used,
            reason = "the entry under a type's id is only ever made as that type"
        )]
        let value = value
            .downcast_mut()
            .expect("an extension is kept under its own type");
        value
    }
}
