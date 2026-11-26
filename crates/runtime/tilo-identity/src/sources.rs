//! The boundaries a host serves an app's identity through: the committed workspaces a request is
//! authorized against, the key service that hands a route its keys, and the person-record kinds
//! an app's surface declares. Every host runs them; the Unix broker below is one host's.

use tilo_desktop_model::{Workspace, WorkspaceId};

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

    /// The ids `app_id` had before, as the installed catalogue believes them (the same publisher
    /// signed them, `app_catalogue::apps::earlier_ids`): the app keeps using the records of
    /// those ids, so a route of one is its launch's own. None by default.
    fn earlier_ids(&self, _app_id: &str) -> Vec<String> {
        Vec::new()
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

#[cfg(unix)]
/// Who a request to the identity service is decided as, and whose device signs it.
pub struct Presented {
    /// The person the workspace decides the request as.
    pub person: [u8; 32],
    /// What the workspace allows them now, when the source decides it itself; `None` leaves it
    /// to the workspace's policy for the person.
    pub access: Option<tilo_action_policy::MemberAccess>,
    /// Whether this device is one of the person's own: what is theirs alone is reached through
    /// such a device only.
    pub their_device: bool,
    /// The person's identity, on a device of theirs.
    pub profile: Option<crate::store::IdentityProfile>,
}

#[cfg(unix)]
/// Why a request is decided as nobody.
#[derive(Debug)]
pub enum Undecided {
    /// This device has no identity to decide it with: a fault of the device, not the request's.
    Fault(String),
    /// Nobody who may make the request asked for it, as a person may read.
    Refused(String),
}

#[cfg(unix)]
/// Who this device signs as for the requests of its apps, and who each request is decided as.
pub trait SignerSource: collab_types::signer::MaybeSendSync + 'static {
    /// Who this device is in `workspace` when it acts for nobody in particular: joining the
    /// workspace's collaboration, and saying who it is.
    ///
    /// # Errors
    /// The device has no identity in `workspace`, as a person may read.
    fn presented(
        &self,
        tree: &dyn WorkspaceMembership,
        workspace: &WorkspaceId,
    ) -> Result<Presented, String>;

    /// Who a request `app` makes in `workspace` is decided as, for the call `call` it names when
    /// it names one.
    ///
    /// # Errors
    /// Nobody this device may act for asked for it, or this device has no identity there.
    fn decided_as(
        &self,
        tree: &dyn WorkspaceMembership,
        workspace: &WorkspaceId,
        app: &str,
        call: Option<[u8; 16]>,
    ) -> Result<Presented, Undecided>;

    /// The key and certificate this device signs with for `presented`, while it is trusted to.
    ///
    /// # Errors
    /// This device is not, or no longer, trusted to sign.
    fn device(
        &self,
        presented: &Presented,
    ) -> std::io::Result<(ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert)>;

    /// Whether this device may hand the keys of the workspace `launch` to a request decided as
    /// `presented`.
    ///
    /// # Errors
    /// It may not, as a person may read.
    fn hands_keys(&self, presented: &Presented, launch: &str) -> Result<(), String>;
}

#[cfg(unix)]
/// A device of a person's own: it signs as the identity its person chose for each workspace,
/// and decides every request as that person.
pub struct PersonDevice;

#[cfg(unix)]
impl SignerSource for PersonDevice {
    fn presented(
        &self,
        tree: &dyn WorkspaceMembership,
        workspace: &WorkspaceId,
    ) -> Result<Presented, String> {
        let profile = crate::broker::identity_in(tree, workspace)?;
        Ok(Presented {
            person: profile.user_public_key,
            access: None,
            their_device: true,
            profile: Some(profile),
        })
    }

    fn decided_as(
        &self,
        tree: &dyn WorkspaceMembership,
        workspace: &WorkspaceId,
        _app: &str,
        _call: Option<[u8; 16]>,
    ) -> Result<Presented, Undecided> {
        self.presented(tree, workspace).map_err(Undecided::Fault)
    }

    fn device(
        &self,
        presented: &Presented,
    ) -> std::io::Result<(ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert)> {
        let profile = presented
            .profile
            .as_ref()
            .ok_or_else(|| std::io::Error::other("this device has no identity to sign in with"))?;
        crate::broker::require_trusted_current_device(profile)
    }

    fn hands_keys(&self, presented: &Presented, launch: &str) -> Result<(), String> {
        // The same trust check as signing, served from memory while none of the files it reads
        // changed (`join_proof`): one `stat` of each instead of reading the device key and
        // certificate. A changed, unreadable or just-written file, a revocation included, is
        // checked again from disk; a refusal is never remembered. No key leaves the check.
        match crate::join_proof::trusted_current_join_proof(launch) {
            Ok(trusted) if trusted.certificate.user_public_key == presented.person => Ok(()),
            Ok(_) => Err("this device certificate does not belong to the selected identity".into()),
            Err(error) => Err(error.to_string()),
        }
    }
}
