//! The boundaries a host serves an app's identity through: the committed workspaces a request is
//! authorized against, the key service that hands a route its keys, the person's devices as the
//! host keeps them, and the person-record kinds an app's surface declares. Every host runs them;
//! the Unix broker below is one host's.

use std::sync::{Arc, OnceLock};

use tilo_desktop_model::{Workspace, WorkspaceId};
use tilo_user_identity::{DeviceCert, RevocationEntry};

/// The committed workspaces the broker authorizes each request against.
pub trait WorkspaceMembership: collab_types::signer::MaybeSendSync + 'static {
    /// Run `read` over the workspace with this id, when this host has it.
    fn with_workspace(&self, workspace: &WorkspaceId, read: &mut dyn FnMut(&Workspace));
}

/// A route's access, generation and content keys, as [`GroupKeySource`]
/// answers; the keys are empty unless newly ready.
pub type GroupKeys = (tilo_identity_client::wire::KeyAccess, u64, Vec<u8>);

/// The key service that hands apps their workspace's content keys.
pub trait GroupKeySource: collab_types::signer::MaybeSendSync + 'static {
    /// The access this device has to `workspace`'s keys, its generation, and
    /// the content keys when ready and newer than `known_generation`; `None`
    /// while no key service runs.
    fn access(
        &self,
        workspace: WorkspaceId,
        known_generation: u64,
    ) -> Option<Result<GroupKeys, String>>;

    /// Keys for an admitted app document. Services without document groups
    /// continue to serve workspace keys.
    fn document_access(
        &self,
        workspace: WorkspaceId,
        _app: &str,
        _document: &str,
        known_generation: u64,
    ) -> Option<Result<GroupKeys, String>> {
        self.access(workspace, known_generation)
    }
}

/// The person's devices as the running host knows them: the keys their root certified and the
/// ones it took back, from the record of their own workspace and the revocations their hosts
/// keep.
///
/// A host installs its answer once ([`install_device_trust`]); this crate asks it wherever it
/// admits a device, so a device the person revoked is refused at the next call and nothing keeps
/// a second copy of their list.
pub trait DeviceTrust: collab_types::signer::MaybeSendSync + 'static {
    /// Whether the root of `person` took `device` back; `None` where this host does not know
    /// their devices.
    fn revoked(&self, person: &[u8; 32], device: &[u8; 32]) -> Option<bool>;

    /// Whether the record of `person`'s devices holds the key `cert` names; `None` where this
    /// host does not know their devices.
    fn enrolled(&self, cert: &DeviceCert) -> Option<bool>;

    /// The revocations the root of `person` signed, as this host keeps them.
    fn revocations(&self, person: &[u8; 32]) -> Vec<RevocationEntry> {
        let _ = person;
        Vec::new()
    }

    /// A number that changes whenever what this answers changes: a cache of something derived
    /// from the person's devices is dropped when it moves.
    fn generation(&self) -> u64 {
        0
    }
}

static DEVICE_TRUST: OnceLock<Arc<dyn DeviceTrust>> = OnceLock::new();

/// Answer this crate's questions about the person's devices through `trust` from now on. A host
/// installs its answer once, as it opens the workspaces holding their record.
pub fn install_device_trust(trust: Arc<dyn DeviceTrust>) {
    let _ = DEVICE_TRUST.set(trust);
}

/// What this host knows of the person's devices, when one is installed.
#[must_use]
pub fn device_trust() -> Option<&'static Arc<dyn DeviceTrust>> {
    DEVICE_TRUST.get()
}

/// A device trust a test drives itself: the keys it is told are enrolled or revoked, and the
/// generation it bumps. One per test binary, so tests use distinct keys.
#[cfg(any(test, feature = "test-support"))]
pub mod test_trust {
    use std::collections::BTreeSet;
    use std::sync::{Arc, LazyLock, Mutex, OnceLock};

    use super::DeviceTrust;
    use tilo_user_identity::{DeviceCert, RevocationEntry};

    #[derive(Default)]
    struct State {
        enrolled: BTreeSet<([u8; 32], [u8; 32])>,
        revoked: BTreeSet<([u8; 32], [u8; 32])>,
        revocations: Vec<RevocationEntry>,
        generation: u64,
    }

    static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

    struct Fake;

    impl DeviceTrust for Fake {
        fn revoked(&self, person: &[u8; 32], device: &[u8; 32]) -> Option<bool> {
            let state = STATE.lock().ok()?;
            Some(state.revoked.contains(&(*person, *device)))
        }

        fn enrolled(&self, cert: &DeviceCert) -> Option<bool> {
            let state = STATE.lock().ok()?;
            Some(
                state
                    .enrolled
                    .contains(&(cert.user_public_key, cert.device_public_key)),
            )
        }

        fn revocations(&self, person: &[u8; 32]) -> Vec<RevocationEntry> {
            STATE
                .lock()
                .map(|state| {
                    state
                        .revocations
                        .iter()
                        .filter(|entry| entry.user_public_key == *person)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        }

        fn generation(&self) -> u64 {
            STATE.lock().map_or(0, |state| state.generation)
        }
    }

    /// Install the fake, if no test in this binary did.
    pub fn install() {
        static INSTALLED: OnceLock<()> = OnceLock::new();
        INSTALLED.get_or_init(|| super::install_device_trust(Arc::new(Fake)));
    }

    /// Say that `person`'s devices record holds `device`, as it would after a certification.
    pub fn enrol(person: [u8; 32], device: [u8; 32]) {
        if let Ok(mut state) = STATE.lock() {
            state.enrolled.insert((person, device));
            state.generation = state.generation.saturating_add(1);
        }
    }

    /// Say that the root of `person` took `device` back, as their record's mark says.
    pub fn revoke(person: [u8; 32], device: [u8; 32]) {
        if let Ok(mut state) = STATE.lock() {
            state.revoked.insert((person, device));
            state.generation = state.generation.saturating_add(1);
        }
    }

    /// Keep `entry` as a revocation this host holds.
    pub fn keep_revocation(entry: RevocationEntry) {
        if let Ok(mut state) = STATE.lock() {
            state.revocations.push(entry);
            state.generation = state.generation.saturating_add(1);
        }
    }
}

/// The records of the person's own an app keeps in, or reads from, their
/// store: the person-record kinds its surface declares. Nothing broader is
/// signed for an app in the person's store.
pub trait PersonResourceTypes: collab_types::signer::MaybeSendSync + 'static {
    /// The person-record kinds `app_id` declares; empty for none.
    fn person_resource_types(&self, app_id: &str) -> Vec<String>;

    /// `app_id` attached naming `surface`, the surface file it was started
    /// with. A host that runs no package of the app may take it as the
    /// app's surface; by default it is ignored and only a package's counts.
    fn attached(&self, _app_id: &str, _surface: &[u8]) {}

    /// Whether `reader` declares that it reads `owner`'s records (its surface's `reads` against
    /// the owner's resource kinds), as the broker admits its read-only join: it is handed the
    /// keys that open them, and signs nothing there. None by default.
    fn reads_documents_of(&self, _reader: &str, _owner: &str) -> bool {
        false
    }
}

/// A host that knows no app's person records: nothing is signed in the
/// person's store.
impl PersonResourceTypes for () {
    fn person_resource_types(&self, _app_id: &str) -> Vec<String> {
        Vec::new()
    }
}

/// A host without a key service: every workspace is unavailable.
impl GroupKeySource for () {
    fn access(
        &self,
        _workspace: WorkspaceId,
        _known_generation: u64,
    ) -> Option<Result<GroupKeys, String>> {
        None
    }
}
