//! What a device whose engine holds its workspaces' records in its own replica reads of them: the
//! page and the sessions a device runs without a workspace service of its own, and the command
//! line's peer, which mirrors them into a folder. A desktop session reads and writes its
//! workspaces' records through the device's workspace service instead.

use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::owner::Holding;

use super::{Admission, Engine, MAX_DOCUMENT_BYTES, ResourceReading};

impl Engine {
    /// What the workspace `workspace` holds of the app `app`'s `record`: the document
    /// as the workspace last admitted it, also what it held before this device joined.
    /// A sealed record opens with this device's channel keys, which reach back past the
    /// membership it joined in; a device that holds none yet asks for them
    /// ([`Self::seal_keys`]) and reads the record once they are here. A record the
    /// hosts send only once asked is asked for on its first read.
    /// # Errors
    /// The workspace is not held here, this device may not read the record, the record
    /// is sealed and this device holds no keys of it, or the document cannot be read.
    pub fn record(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(
            match self.read_resource(
                workspace,
                (app, record),
                Holding::default(),
                MAX_DOCUMENT_BYTES,
            )? {
                ResourceReading::Captured { bytes, .. } => Some(bytes),
                ResourceReading::Changes { .. }
                | ResourceReading::Unchanged(_)
                | ResourceReading::Pending => None,
            },
        )
    }

    /// What `workspace` holds of `app`'s `record` for a reader that merged it last at the sorted
    /// `heads` an earlier read handed out, or holds nothing of it: only the changes since those
    /// heads where the record holds them ([`ResourceReading::Changes`]), the record whole where it
    /// does not, so a reader that follows a record merges what changed rather than all of it.
    /// # Errors
    /// As [`Self::record`].
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
    pub fn read_resource(
        &self,
        workspace: &str,
        (app, record): (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
    ) -> Result<ResourceReading, String> {
        let Some(held) = self.own_writes(workspace, app, record) else {
            return self.read_replica(workspace, (app, record), holding, maximum);
        };
        let id = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        let own = self.identity.device_cert.device_public_key;
        self.owner.with_current(id, |view| {
            view.peer().check_document_read(
                Admission::of(view, own).reader(own),
                app,
                record,
                tilo_clock::wall(),
            )
        })?;
        if let Some(reading) =
            self.read_own_since(workspace, (app, record), holding, maximum, &held)?
        {
            return Ok(reading);
        }
        // This device's own writes the replica has not taken in yet read over what it holds.
        let base = match self.read_replica(workspace, (app, record), Holding::default(), maximum) {
            Ok(ResourceReading::Captured { bytes, .. }) => Some(bytes),
            _ => None,
        };
        let author = self.identity.device_cert.device_public_key;
        let tilo_peer_state::document_frame::OwnRead {
            bytes,
            heads,
            revision,
        } = tilo_peer_state::document_frame::with_own_writes(base.as_deref(), author, &held)?;
        if holding.revision == Some(revision) {
            return Ok(ResourceReading::Unchanged(revision));
        }
        Ok(ResourceReading::Captured {
            revision,
            heads,
            bytes,
        })
    }

    /// [`Self::read_resource`] of a record this device wrote to and its replica has not taken in yet
    /// (`held`), for a reader `holding` heads of an earlier read: what the replica changed since what
    /// the reader holds of it, then those of the writes the reader lacks, so a record that
    /// changes while this device's writes wait is never read whole. `None` where only a whole read
    /// answers: the reader holds no heads, or one the replica and the writes do not, or a write
    /// carries the record whole.
    fn read_own_since(
        &self,
        workspace: &str,
        route: (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
        held: &[Vec<u8>],
    ) -> Result<Option<ResourceReading>, String> {
        if holding.heads.is_empty() {
            return Ok(None);
        }
        let author = self.identity.device_cert.device_public_key;
        let Ok(Some(own)) = tilo_peer_state::document_frame::own_changes(author, held) else {
            return Ok(None);
        };
        let mut failed = None;
        let read = tilo_peer_state::document_frame::read_after_own(holding.heads, &own, |base| {
            let replica = self.read_replica(
                workspace,
                route,
                Holding {
                    revision: Some(tilo_peer_state::owner::revision_of_heads(base)),
                    heads: base,
                },
                maximum,
            );
            match replica {
                Ok(ResourceReading::Changes { bytes, heads, .. }) => Some((bytes, heads)),
                Ok(ResourceReading::Unchanged(_)) => Some((Vec::new(), base.to_vec())),
                Ok(ResourceReading::Captured { .. } | ResourceReading::Pending) => None,
                Err(error) => {
                    failed = Some(error);
                    None
                }
            }
        });
        if let Some(error) = failed {
            return Err(error);
        }
        let Some((bytes, heads)) = read.filter(|(bytes, _)| bytes.len() <= maximum) else {
            return Ok(None);
        };
        let revision = tilo_peer_state::owner::revision_of_heads(&heads);
        if bytes.is_empty() && heads.as_slice() == holding.heads {
            return Ok(Some(ResourceReading::Unchanged(revision)));
        }
        Ok(Some(ResourceReading::Changes {
            revision,
            heads,
            bytes,
        }))
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
    /// (`neighbour_wire::app_document_channel`): what they hold of it, and their changes to it from
    /// then on. Asked once per record while the peer network runs; `false` when there is no peer network to ask
    /// on, so nothing was asked.
    pub fn open_resource(&self, workspace: &str, app: &str, record: &str) -> bool {
        let Some(workspace) = WorkspaceId::parse(workspace) else {
            return false;
        };
        let Some(link) = self.link.lock_recover().clone() else {
            return false;
        };
        let own = tilo_server_protocol::OwnedChannel::recognise(
            app,
            record,
            self.identity.device_cert.user_public_key,
        );
        link.open_resource(
            workspace,
            neighbour_wire::app_document_channel(app, record),
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
        let channel = neighbour_wire::app_document_channel(app, record);
        self.link
            .lock_recover()
            .as_ref()?
            .resource_answer(workspace, &channel)
    }

    /// A live local document read check for consumers of cached copies.
    pub fn document_read_policy(
        &self,
        workspace: WorkspaceId,
    ) -> impl Fn(&str, &str) -> bool + use<> {
        let owner = self.owner.clone();
        let own = self.identity.device_cert.device_public_key;
        move |app, document_id| {
            owner
                .with_current(workspace, |view| {
                    let reader = Admission::of(view, own).reader(own);
                    view.peer()
                        .check_document_read(reader, app, document_id, tilo_clock::wall())
                })
                .is_ok()
        }
    }

    /// Every document the replica holds for `workspace`, by its app and id. A workspace this device
    /// does not hold lists nothing.
    /// # Errors
    /// The workspace is not held here, or its documents cannot be listed now.
    pub fn documents(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let id = WorkspaceId::parse(workspace).ok_or("that is not a workspace")?;
        let held: Vec<_> = self
            .held
            .lock()
            .keys()
            .filter(|route| route.cluster_id == workspace)
            .map(|route| (route.app_id.clone(), route.document.clone()))
            .collect();
        let own = self.identity.device_cert.device_public_key;
        let now = tilo_clock::wall();
        self.owner.with_current(id, |view| {
            let reader = Admission::of(view, own).reader(own);
            let mut documents = view.peer().records();
            // Local writes become visible before their replica admits them.
            for document_id in &held {
                if !documents.contains(document_id) {
                    documents.push(document_id.clone());
                }
            }
            documents.retain(|(app, document_id)| {
                view.peer()
                    .check_document_read(reader, app, document_id, now)
                    .is_ok()
            });
            Ok(documents)
        })
    }
}
