//! Per-workspace routing and authorization metadata: who is a member, what each
//! member device may do, and who the workspace is attached to. A workspace's changes
//! are not here: they are the device store's, and reach devices over its link.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::workspace_files::WorkspaceFiles;
use tilo_server_protocol::WorkspaceId;

/// Whether `next` leaves out a device `previous` lists: only such a list narrows anyone's
/// access, and wakes the links admitted under it to re-check
/// (`crate::authority_epoch`). One that only adds devices wakes nobody: a public workspace
/// admitting a crowd would otherwise re-audit every member's link for every newcomer.
pub(crate) fn leaves_out(previous: &[[u8; 32]], next: &[[u8; 32]]) -> bool {
    let kept: std::collections::HashSet<&[u8; 32]> = next.iter().collect();
    previous.iter().any(|device| !kept.contains(device))
}

/// The access of each member device of a workspace, by device key.
pub type DeviceAccess = std::collections::BTreeMap<[u8; 32], tilo_action_policy::MemberAccess>;

/// Per-workspace runtime.
pub struct WorkspaceRuntime {
    /// The workspace this runtime is of, by the one id a workspace has.
    workspace_id: WorkspaceId,
    /// Whether the workspace's governor follows its app surfaces arriving.
    pub(crate) followed: bool,
    /// The in-process authority serving this workspace, if any: it answers
    /// the workspace's calls.
    pub workspace_authority: Option<crate::workspaces::WorkspaceAuthority>,
    /// The access of each member device, as the workspace's signed member list
    /// gives it: what the server decides each change's claimed action against.
    /// `None` until the list settled here; the server then decides changes
    /// against their app surface alone.
    pub(crate) device_access: Option<Arc<DeviceAccess>>,
    /// The workspace's files on this host.
    pub files: WorkspaceFiles,
    /// The workspace left the host's registry ([`crate::workspace_io::evict`]):
    /// its files stay, and this runtime writes them no more.
    pub(crate) evicted: bool,
    /// Who the workspace's signed member list has now: its members by their roots
    /// and their devices, with the browser environments this server placed in it
    /// ([`crate::environment`]). Set when the list settles here and rebuilt from the
    /// durable list when the workspace is bound; never kept on disk. Both lists are
    /// sorted.
    pub(crate) members: Arc<tilo_peer_state::GovernedAccess>,
    /// The devices this workspace keeps as keys its person revoked: refused with how to approve
    /// another wherever they ask, until their person's approval admits a new key.
    pub(crate) revoked_keys: HashSet<[u8; 32]>,
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

    /// Whether the person whose root is `user` is a member of this workspace: what
    /// [`crate::workspace_access`] enrols their devices by.
    #[must_use]
    pub fn admits_person(&self, user: &[u8; 32]) -> bool {
        self.members.member_user_roots.binary_search(user).is_ok()
    }

    /// Whether the workspace's member list has `device`.
    #[must_use]
    pub fn lists(&self, device: &[u8; 32]) -> bool {
        self.members.member_devices.binary_search(device).is_ok()
    }

    /// Take `members` as who the workspace has now, `revoked` as the keys its people revoked,
    /// and `device_access` as what each member device may do. A list that leaves a device out
    /// refuses its links, which re-check on the announced change.
    pub(crate) fn settle_members(
        &mut self,
        mut members: tilo_peer_state::GovernedAccess,
        revoked: impl IntoIterator<Item = [u8; 32]>,
        device_access: Option<DeviceAccess>,
    ) {
        members.member_devices.sort_unstable();
        members.member_devices.dedup();
        members.member_user_roots.sort_unstable();
        members.member_user_roots.dedup();
        self.revoked_keys.extend(revoked);
        self.revoked_keys
            .retain(|device| members.member_devices.binary_search(device).is_err());
        let device_removed = self
            .members
            .member_devices
            .iter()
            .copied()
            .any(|device| members.member_devices.binary_search(&device).is_err());
        let roots_left = leaves_out(&self.members.member_user_roots, &members.member_user_roots);
        self.members = Arc::new(members);
        if let Some(device_access) = device_access {
            self.device_access = Some(Arc::new(device_access));
        }
        if device_removed || roots_left {
            crate::authority_epoch::bump();
        }
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
            members: Arc::default(),
            revoked_keys: HashSet::new(),
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
