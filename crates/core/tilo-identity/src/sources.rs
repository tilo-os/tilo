//! The boundaries a host serves an app's identity through: the committed workspaces a request is
//! authorized against, the key service that hands a route its keys, the person's devices as the
//! host keeps them, and the person-record kinds an app's surface declares. Every host runs them;
//! the Unix broker below is one host's.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use tilo_desktop_model::{Workspace, WorkspaceId};
use tilo_lock::RwLockRecover as _;
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

/// The person's devices as the running host knows them: the keys their root certified and the ones
/// it took back, from the record of their own workspace, each revocation the root's own signature
/// verified there.
///
/// A host installs its answer once ([`install_device_trust`]); this crate asks it wherever it
/// admits a device, so a device the person revoked is refused at the next call and nothing keeps
/// a second copy of their list. It lives in a process-wide static, so it is `Send + Sync` on every
/// target, the browser too.
pub trait DeviceTrust: Send + Sync + 'static {
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

    /// Whether the record of `cert`'s person's devices lets its key reach the workspace
    /// `workspace` names (its UUID): `false` for a key their root confined to another workspace
    /// alone, as its entry there says; `None` where this host does not know their devices.
    fn reaches(&self, cert: &DeviceCert, workspace: &str) -> Option<bool> {
        let _ = (cert, workspace);
        None
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

/// The person's devices as a host read them from the record that holds them: the keys their root
/// certified and the revocations it signed, for `person`.
///
/// A host gathers one of these from its own copy of their record; [`DevicesTrust`] answers every
/// admission from it, so a daemon that keeps the record and a session that reads the record from
/// that daemon answer the same way. A revocation is the root's own signature, checked as the record
/// is read; one that does not verify is left out and refuses nothing.
#[derive(Clone, Debug)]
pub struct DevicesSeen {
    /// The person whose devices were read.
    pub person: [u8; 32],
    /// The keys their root certified, as the record lists them.
    pub certified: BTreeSet<[u8; 32]>,
    /// The revocations their root signed, as the record holds them: the keys they name are the
    /// ones this host refuses.
    pub revocations: Vec<RevocationEntry>,
    /// The keys their root confined to one workspace alone (a workspace's VM), each with that
    /// workspace's id, as the certificate in the record's entry says: refused in every other.
    pub confined: std::collections::BTreeMap<[u8; 32], [u8; 16]>,
}

impl DevicesSeen {
    /// What a host holds of `person`'s devices: the keys the record lists, and the revocations
    /// their root signed there. An entry that root did not sign, or one about another person, is
    /// dropped here, so nothing below this seam can take an unverified claim for a revocation.
    #[must_use]
    pub fn of_host(
        person: [u8; 32],
        certified: impl IntoIterator<Item = [u8; 32]>,
        revocations: impl IntoIterator<Item = RevocationEntry>,
    ) -> Self {
        Self {
            person,
            certified: certified.into_iter().collect(),
            revocations: revocations
                .into_iter()
                .filter(|entry| {
                    entry.user_public_key == person
                        && tilo_user_identity::verify_revocation_entry(entry).is_ok()
                })
                .collect(),
            confined: std::collections::BTreeMap::new(),
        }
    }

    /// The same, with the keys the record's entries confine to one workspace alone, each with
    /// that workspace's id, as the certificates their root signed there say.
    #[must_use]
    pub fn confining(mut self, confined: impl IntoIterator<Item = ([u8; 32], [u8; 16])>) -> Self {
        self.confined.extend(confined);
        self
    }

    /// Whether the record lets `device` reach the workspace `workspace` names (its UUID): every
    /// key reaches every workspace but one confined to another.
    #[must_use]
    pub fn reaches(&self, device: &[u8; 32], workspace: &str) -> bool {
        self.confined.get(device).is_none_or(|only| {
            tilo_user_identity::DeviceCapabilities::workspace_device(*only)
                .reaches_workspace(workspace)
        })
    }

    /// Whether the root of `person` took `device` back, as the entries the record holds say.
    #[must_use]
    pub fn revoked(&self, device: &[u8; 32]) -> bool {
        self.revocations
            .iter()
            .any(|entry| entry.revoked_device_public_key == *device)
    }

    /// A number that changes whenever what this answers changes: a cache of something derived
    /// from the person's devices is dropped when it moves.
    #[must_use]
    pub fn generation(&self) -> u64 {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.person);
        for key in &self.certified {
            hasher.update(b"certified");
            hasher.update(key);
        }
        for (key, workspace) in &self.confined {
            hasher.update(b"confined");
            hasher.update(key);
            hasher.update(workspace);
        }
        // The host keeps revocations in whatever order it read them: what they say, not the
        // order, is what this number follows.
        let mut revocations: Vec<&RevocationEntry> = self.revocations.iter().collect();
        revocations.sort_by_key(|entry| {
            (
                entry.user_public_key,
                entry.revoked_device_public_key,
                entry.revoked_at_ns,
            )
        });
        for entry in revocations {
            hasher.update(b"revocation");
            hasher.update(&entry.user_public_key);
            hasher.update(&entry.revoked_device_public_key);
            hasher.update(&entry.revoked_at_ns.to_le_bytes());
        }
        let digest = hasher.finalize();
        digest
            .as_bytes()
            .get(..8)
            .and_then(|head| head.try_into().ok())
            .map_or(0, u64::from_le_bytes)
    }
}

/// The one [`DeviceTrust`] a host serves: it answers every question from the person's devices as
/// its [`DevicesSeen`] says them, and the host fills that from the record it keeps.
///
/// A daemon keeps the record and reads it on every call; a desktop session reads it from the
/// daemon and keeps its last reading, filling it again whenever the record moves. Both install
/// this type, so a device is admitted wherever the host that holds the record says it is.
#[derive(Default)]
pub struct DevicesTrust {
    read: std::sync::RwLock<Option<DeviceRead>>,
}

/// How a host reads the person's devices, when it can read them now.
type DeviceRead = Box<dyn Fn() -> Option<DevicesSeen> + Send + Sync>;

impl DevicesTrust {
    /// A trust that answers nothing until it is filled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer from now on with what `read` says, in place of what an earlier fill said: a host
    /// that reads the person's devices another way, or again after they moved, says so here.
    pub fn reading(&self, read: impl Fn() -> Option<DevicesSeen> + Send + Sync + 'static) {
        *self.read.write_recover() = Some(Box::new(read));
    }

    /// What this answers now, when it has been filled.
    fn seen(&self) -> Option<DevicesSeen> {
        let read = self.read.read_recover();
        (read.as_ref()?)()
    }
}

impl DeviceTrust for DevicesTrust {
    fn revoked(&self, person: &[u8; 32], device: &[u8; 32]) -> Option<bool> {
        let seen = self.seen()?;
        (seen.person == *person).then(|| seen.revoked(device))
    }

    fn enrolled(&self, cert: &DeviceCert) -> Option<bool> {
        let seen = self.seen()?;
        (seen.person == cert.user_public_key)
            .then(|| seen.certified.contains(&cert.device_public_key))
    }

    fn revocations(&self, person: &[u8; 32]) -> Vec<RevocationEntry> {
        self.seen()
            .filter(|seen| seen.person == *person)
            .map(|seen| seen.revocations)
            .unwrap_or_default()
    }

    fn reaches(&self, cert: &DeviceCert, workspace: &str) -> Option<bool> {
        let seen = self.seen()?;
        (seen.person == cert.user_public_key).then(|| {
            cert.capabilities.reaches_workspace(workspace)
                && seen.reaches(&cert.device_public_key, workspace)
        })
    }

    fn generation(&self) -> u64 {
        self.seen().map_or(0, |seen| seen.generation())
    }
}

/// A device trust a test drives itself: the revocations it is told, and the generation it bumps.
/// It lists every device, as a host that knows the person's record does; what a host with none
/// answers is the rule's own test. One per test binary, so tests use distinct keys.
#[cfg(any(test, feature = "test-support"))]
pub mod test_trust {
    use std::collections::BTreeSet;
    use std::sync::{Arc, LazyLock, Mutex, OnceLock};

    use super::DeviceTrust;
    use tilo_user_identity::{DeviceCert, RevocationEntry};

    #[derive(Default)]
    struct State {
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

        fn enrolled(&self, _cert: &DeviceCert) -> Option<bool> {
            // The fake stands where a host that knows the person answers; which device it would
            // refuse for want of an entry is the rule's own test, not this double's.
            Some(true)
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

    /// What a test says where a device is listed: every device is, to this double.
    pub fn enrol(_person: [u8; 32], _device: [u8; 32]) {
        if let Ok(mut state) = STATE.lock() {
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

    /// `app_id`'s service connection `stream` named `workspace` as the one it was started in,
    /// before anything it asks is answered. A host may make that workspace ready for an app it
    /// launched there: a person's own workspace is founded the first time an app they launched
    /// reaches it. Nothing by default.
    #[cfg(unix)]
    fn connected(
        &self,
        _app_id: &str,
        _workspace: &WorkspaceId,
        _stream: &std::os::unix::net::UnixStream,
    ) {
    }

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

#[cfg(test)]
mod confined_tests {
    use super::{DeviceTrust as _, DevicesSeen, DevicesTrust};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    const OWN: &str = "11111111-1111-4111-8111-111111111111";
    const OTHER: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn own_id() -> [u8; 16] {
        [
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x41, 0x11, 0x81, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11,
        ]
    }

    /// The VM's certificate, which the person's root signed for [`OWN`] alone.
    fn vm_cert() -> DeviceCert {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [9; 32],
            device_label: "Workspace computer".into(),
            capabilities: DeviceCapabilities::workspace_device(own_id()),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    }

    /// The person's devices as a host that holds their record answers, with the VM's entry
    /// confining its key to its workspace: the key is refused in every other, whatever
    /// certificate it shows, and the answer moves when the record does.
    #[test]
    fn the_record_keeps_a_confined_key_out_of_every_other_workspace() {
        let cert = vm_cert();
        let person = cert.user_public_key;
        let trust = DevicesTrust::new();
        let listed = DevicesSeen::of_host(person, [cert.device_public_key], []);
        let unconfined = listed.generation();
        let confined = listed.confining([(cert.device_public_key, own_id())]);
        assert_ne!(
            confined.generation(),
            unconfined,
            "a confinement moves the record's answer"
        );
        trust.reading(move || Some(confined.clone()));
        assert_eq!(trust.reaches(&cert, OWN), Some(true));
        assert_eq!(trust.reaches(&cert, OTHER), Some(false));
        // The same key showing a certificate that claims no confinement is still the record's
        // confined key.
        let mut claimed = cert.clone();
        claimed.capabilities.workspace = None;
        assert_eq!(trust.reaches(&claimed, OTHER), Some(false));
        // Another person's key is no question this record answers.
        let mut stranger = cert;
        stranger.user_public_key = [7; 32];
        assert_eq!(trust.reaches(&stranger, OTHER), None);
    }

    /// Admission into a workspace refuses a confined device everywhere but its workspace, by its
    /// certificate alone where the host knows nothing of the person's devices.
    #[test]
    fn admission_into_a_workspace_refuses_a_device_confined_to_another() {
        let cert = vm_cert();
        let person = cert.user_public_key;
        assert!(crate::certificates::admitted(&person, &cert));
        assert!(crate::certificates::admitted_in(OWN, &person, &cert));
        assert!(!crate::certificates::admitted_in(OTHER, &person, &cert));
        assert!(!crate::certificates::admitted_in(
            "not a workspace",
            &person,
            &cert
        ));
    }
}
