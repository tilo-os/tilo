//! The seams a host serves an app's identity through: the committed workspaces a request is
//! authorized against, the channel service that hands a route its keys, and the person-record kinds
//! an app's surface declares. Every host runs them; the Unix broker below is one host's.

use tilo_desktop_model::{Workspace, WorkspaceId};

/// The committed workspaces the broker authorizes each request against.
pub trait WorkspaceMembership: collab_types::signer::MaybeSendSync + 'static {
    /// Run `read` over the workspace with this id, when this host has it.
    fn with_workspace(&self, workspace: &WorkspaceId, read: &mut dyn FnMut(&Workspace));
}

/// A route's access, generation and content keys, as [`ChannelKeySource`]
/// answers; the keys are empty unless newly ready.
pub type ChannelKeys = (runtime_identity::wire::ChannelAccess, u64, Vec<u8>);

/// The channel service that hands apps their routes' content keys.
pub trait ChannelKeySource: collab_types::signer::MaybeSendSync + 'static {
    /// The access this device has to the channel of `route`'s record in
    /// `workspace`, its generation, and the route's content keys when ready and
    /// newer than `known_generation`; `None` while no channel service runs.
    /// The route goes to the channel's participants with this device's access
    /// request, so it can decide Read on the record itself.
    fn access(
        &self,
        workspace: WorkspaceId,
        route: &collab_types::ChannelRoute,
        known_generation: u64,
    ) -> Option<Result<ChannelKeys, String>>;
}

/// The records of the person's own an app keeps in, or reads from, their
/// store: the person-record kinds its surface declares. Nothing broader is
/// signed for an app in the person's store.
pub trait PersonRecordKinds: collab_types::signer::MaybeSendSync + 'static {
    /// The person-record kinds `app_id` declares; empty for none.
    fn person_record_kinds(&self, app_id: &str) -> Vec<String>;

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
    fn reads_records_of(&self, _reader: &str, _owner: &str) -> bool {
        false
    }
}

/// A host that knows no app's person records: nothing is signed in the
/// person's store.
impl PersonRecordKinds for () {
    fn person_record_kinds(&self, _app_id: &str) -> Vec<String> {
        Vec::new()
    }
}

/// A host without a channel service: every route is unavailable.
impl ChannelKeySource for () {
    fn access(
        &self,
        _workspace: WorkspaceId,
        _route: &collab_types::ChannelRoute,
        _known_generation: u64,
    ) -> Option<Result<ChannelKeys, String>> {
        None
    }
}
