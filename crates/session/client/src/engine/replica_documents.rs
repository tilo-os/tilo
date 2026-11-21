//! What the command line's peer reads of the workspaces its engine holds in its own replica: it
//! mirrors them into a folder, and reads what changed since the heads it holds. A page reads and
//! writes through its device's holder ([`super::documents`]) and a desktop session through the
//! device's workspace service; neither reads its replica through here.

use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::owner::Holding;

use super::{Engine, MAX_DOCUMENT_BYTES, ResourceReading};

impl Engine {
    /// What `workspace` holds of `app`'s `record` for a reader that merged it last at the sorted
    /// `heads` an earlier read handed out, or holds nothing of it: only the changes since those
    /// heads where the record holds them ([`ResourceReading::Changes`]), the record whole where it
    /// does not, so a reader that follows a record merges what changed rather than all of it.
    /// # Errors
    /// The workspace is not held here, this device may not read the record, the record is sealed
    /// and this device holds no keys of it, or the document cannot be read.
    pub fn document_update(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
        heads: Option<&[[u8; 32]]>,
    ) -> Result<ResourceReading, String> {
        let heads = heads.unwrap_or_default();
        self.read_resource(
            workspace,
            (app, record),
            Holding {
                revision: (!heads.is_empty())
                    .then(|| tilo_peer_state::owner::revision_of_heads(heads)),
                heads,
            },
            MAX_DOCUMENT_BYTES,
        )
    }

    /// What `workspace` holds of `app`'s `record` now, for the reader this device is: at most
    /// `maximum` bytes of it, and the heads it stands at; for a reader `holding` heads an earlier
    /// read handed it, only the changes since.
    ///
    /// # Errors
    /// The workspace is not held here, this device may not read the record, the record is sealed
    /// and this device holds no keys of it, or the document cannot be read.
    pub(crate) fn read_resource(
        &self,
        workspace: &str,
        (app, record): (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
    ) -> Result<ResourceReading, String> {
        self.read_replica(workspace, (app, record), holding, maximum)
    }

    /// A digest of what this device's replica admitted of `workspace`'s `app`'s `document`, sealed
    /// or in the clear, reading and opening nothing: equal at two moments, nothing was admitted
    /// to it between them, so a reader that read it at the first need not read it again. `None`
    /// for a document not held here. It says nothing of who may read it: the read does.
    #[must_use]
    pub fn admitted_digest(&self, workspace: &str, app: &str, document: &str) -> Option<[u8; 32]> {
        let workspace = WorkspaceId::parse(workspace)?;
        self.owner
            .with_current(workspace, |view| {
                Ok(view.peer().document_revision(app, document))
            })
            .ok()
            .flatten()
    }

    /// Whether `workspace` holds `app`'s `record` here now: what a read of it opens, or a record
    /// this device holds nothing of that may still be on its way.
    #[must_use]
    pub fn holds_resource(&self, workspace: &str, app: &str, record: &str) -> bool {
        WorkspaceId::parse(workspace).is_some_and(|workspace| {
            self.owner.with_current(workspace, |view| {
                Ok(view.peer().document(app, record).is_some())
            }) == Ok(true)
        })
    }

    /// Ask `workspace`'s hosts for `app`'s `record`
    /// (`neighbour_wire::app_document_group`, the app's document group): what they hold of it, and their changes to it from
    /// then on. Asked once per record while the peer network runs; `false` when there is no peer network to ask
    /// on, so nothing was asked.
    pub fn open_resource(&self, workspace: &str, app: &str, record: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let Some(link) = self.link.lock_recover().clone() else {
            return false;
        };
        let own = tilo_server_protocol::OwnedGroup::recognise(
            app,
            record,
            self.identity.device_cert.user_public_key,
        );
        link.open_resource(
            workspace,
            neighbour_wire::app_document_group(app, record),
            own,
        )
    }

    /// What `workspace`'s host answered about `app`'s `record`: `None` while the answer to
    /// [`Self::open_resource`] is owed; once it came, the frames it sent — none when the workspace
    /// has none of the record, which is then new here — or that it is another person's own.
    #[must_use]
    pub fn resource_answer(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Option<tilo_server_protocol::DocumentAnswer> {
        let workspace = WorkspaceId::parse(workspace)?;
        let group = neighbour_wire::app_document_group(app, record);
        self.link
            .lock_recover()
            .as_ref()?
            .resource_answer(workspace, &group)
    }
}
