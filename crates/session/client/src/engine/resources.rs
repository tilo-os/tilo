//! A workspace's resources, as this device writes and reads them: through the resource store,
//! which alone decides the documents they are in ([`tilo_resource_store`]).
//!
//! A write goes to the store, which answers the change it made to each document; each is then
//! this device's signed write of that document, under the app whose resource it is, sealed for
//! the document's reader set, and published as any write of this device is
//! ([`Engine::write_resource`]). A read asks the store, which answers only what this device's
//! person may read.
//!
//! The store of a workspace declares the types and verbs of every app's surface this process
//! knows, which a peer's change is judged by. Before serving a read it discovers the documents
//! this device may read, refreshes changed documents, and drops those it can no longer read.

use std::collections::BTreeMap;

use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::owner::{AppWrite, WriteRefusal};
use tilo_resource_store::{Resource, Schema, Stamp, Store, Written};

use super::{Engine, SealKeys};

/// The store of one workspace, and the store documents it read from this device's replica.
#[derive(Default)]
pub(super) struct Resources {
    store: Store,
    /// Each store document read from the replica: the app it is written under, and the revision
    /// it was read at.
    read: BTreeMap<String, (String, Option<[u8; 32]>)>,
    /// How many surfaces this process had named when the store last declared them
    /// ([`collab_types::app_surface::registered`]).
    declared: u64,
}

/// Each change a write made, with its document whole where the replica holds none of it yet.
type Publishing = Vec<(Written, Option<Vec<u8>>)>;

/// The workspaces' stores.
pub(super) type Stores = std::sync::Mutex<BTreeMap<WorkspaceId, Resources>>;

impl Engine {
    /// This device's person, as reader sets name people.
    fn reader(&self) -> String {
        hex::encode(self.identity.device_cert.user_public_key)
    }

    /// Calls `f` with `workspace`'s store, brought up to what this device's replica holds.
    fn with_resources<R>(&self, workspace: WorkspaceId, f: impl FnOnce(&mut Resources) -> R) -> R {
        let id = workspace.to_string();
        let mut stores = self.stores.lock_recover();
        let resources = stores.entry(workspace).or_default();
        for (app, doc) in self.documents(&id).unwrap_or_default() {
            if collab_types::is_store_document(&doc) {
                resources.read.entry(doc).or_insert((app, None));
            }
        }
        // Every app's types and verbs this process knows, which a peer's change is judged by.
        let named = collab_types::app_surface::registered();
        if resources.declared != named {
            resources.declared = named;
            for (app, bytes) in collab_types::app_surface::every() {
                match collab_types::surface::SurfaceFile::parse(bytes) {
                    Ok(file) => {
                        for surface in &file.resources {
                            resources.store.declare(Schema::from_surface(surface));
                        }
                    }
                    Err(error) => tracing::warn!(%error, %app, "an app's surface is not declared"),
                }
            }
        }
        // What the replica took since the store read it.
        let documents: Vec<(String, String, Option<[u8; 32]>)> = resources
            .read
            .iter()
            .map(|(doc, (app, revision))| (doc.clone(), app.clone(), *revision))
            .collect();
        let may_read = self.document_read_policy(workspace);
        for (doc, app, revision) in documents {
            if !may_read(&app, &doc) {
                resources.store.forget_document(&doc);
                resources.read.insert(doc, (app, None));
                continue;
            }
            let now = self.document_revision(&id, &app, &doc).ok().flatten();
            if now.is_none() || now == revision {
                continue;
            }
            let read = self.read_resource(
                &id,
                (&app, &doc),
                tilo_peer_state::owner::Holding::default(),
                super::MAX_DOCUMENT_BYTES,
            );
            match read {
                Ok(super::ResourceReading::Captured { bytes, .. }) => {
                    if let Err(error) = resources.store.load(&doc, &bytes) {
                        tracing::warn!(%error, %doc, "a store document is not read");
                        continue;
                    }
                    resources.read.insert(doc, (app, now));
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, %doc, "a store document is not read"),
            }
        }
        let answer = f(resources);
        drop(stores);
        answer
    }

    /// Reads `workspace`'s resources: `f` is given its store and the reader check this device's
    /// person answers, which every query, count and read of it takes.
    pub fn read_resources<R>(
        &self,
        workspace: WorkspaceId,
        f: impl FnOnce(&Store, &dyn Fn(&str) -> bool) -> R,
    ) -> R {
        let reader = self.reader();
        let may_read = move |readers: &str| {
            readers == "*" || readers.split(',').any(|person| person == reader)
        };
        self.with_resources(workspace, |resources| f(&resources.store, &may_read))
    }

    /// Writes `resource` as the resource `id` of `kind`, of `app`, in `workspace`, as the action
    /// `stamp` names, and publishes each document's change as this device's write.
    ///
    /// # Errors
    /// The store refused the write, the keys of a document's reader set are not here, or the
    /// workspace refused a document's write.
    pub async fn put_resource(
        &self,
        workspace: WorkspaceId,
        app: &str,
        (kind, id): (&str, &str),
        resource: Resource,
        stamp: &Stamp,
    ) -> Result<Vec<AppWrite>, WriteRefusal> {
        let written =
            self.write_store(workspace, app, |store| store.put(kind, id, resource, stamp))?;
        self.publish_written(workspace, app, written).await
    }

    /// Removes the resource `id` of `kind`, of `app`, from `workspace`, as `stamp` names.
    ///
    /// # Errors
    /// As [`Self::put_resource`].
    pub async fn remove_resource(
        &self,
        workspace: WorkspaceId,
        app: &str,
        (kind, id): (&str, &str),
        stamp: &Stamp,
    ) -> Result<Vec<AppWrite>, WriteRefusal> {
        let written = self.write_store(workspace, app, |store| {
            store.remove(kind, id, stamp).map(|w| vec![w])
        })?;
        self.publish_written(workspace, app, written).await
    }

    /// Takes back the invocation `invocation` in `workspace`'s resources of `app`, in every
    /// document it changed, as `stamp` names: an invocation of its own that names the verb it
    /// takes back and, in `undoes`, the invocation.
    ///
    /// # Errors
    /// As [`Self::put_resource`].
    pub async fn undo_resources(
        &self,
        workspace: WorkspaceId,
        app: &str,
        invocation: &str,
        stamp: &Stamp,
    ) -> Result<Vec<AppWrite>, WriteRefusal> {
        let written = self.write_store(workspace, app, |store| store.undo(invocation, stamp))?;
        self.publish_written(workspace, app, written).await
    }

    /// Edits the content of the resource `id` of `kind`, of `app`, in `workspace`, as `stamp`
    /// names: `edit` is given the root of its content.
    ///
    /// # Errors
    /// As [`Self::put_resource`], and when `edit` fails.
    pub async fn edit_resource_content<R>(
        &self,
        workspace: WorkspaceId,
        app: &str,
        (kind, id): (&str, &str),
        stamp: &Stamp,
        edit: impl FnOnce(
            &mut automerge::transaction::Transaction<'_>,
            &automerge::ObjId,
        ) -> Result<R, automerge::AutomergeError>,
    ) -> Result<(R, Vec<AppWrite>), WriteRefusal> {
        let mut answer = None;
        let written = self.write_store(workspace, app, |store| {
            let (edited, written) = store.edit_content(kind, id, stamp, edit)?;
            answer = Some(edited);
            Ok(written.into_iter().collect())
        })?;
        let writes = self.publish_written(workspace, app, written).await?;
        let answer =
            answer.ok_or_else(|| WriteRefusal::Refused("the content was not edited".to_owned()))?;
        Ok((answer, writes))
    }

    /// Writes to `workspace`'s store with `write`, and answers each document's change with the
    /// document whole where the replica holds none of it yet.
    fn write_store(
        &self,
        workspace: WorkspaceId,
        app: &str,
        write: impl FnOnce(&mut Store) -> Result<Vec<Written>, tilo_resource_store::StoreError>,
    ) -> Result<Publishing, WriteRefusal> {
        let id = workspace.to_string();
        self.with_resources(workspace, |resources| {
            let written = write(&mut resources.store)
                .map_err(|error| WriteRefusal::Refused(error.to_string()))?;
            Ok(written
                .into_iter()
                .map(|write| {
                    resources
                        .read
                        .entry(write.doc.clone())
                        .or_insert_with(|| (app.to_owned(), None));
                    let founding = (!self.holds_resource(&id, app, &write.doc))
                        .then(|| {
                            resources
                                .store
                                .document(&write.doc)
                                .map(automerge::Automerge::save)
                        })
                        .flatten();
                    (write, founding)
                })
                .collect())
        })
    }

    fn store_write_keys(
        &self,
        workspace: WorkspaceId,
        app: &str,
        document: &str,
        readers: &str,
    ) -> Result<SealKeys, String> {
        if readers == collab_types::EVERY_MEMBER {
            return self.resource_write_keys(workspace, (app, document));
        }
        let isolated = self.owner.with_current(workspace, |view| {
            view.peer()
                .requires_document_keys(app, document, tilo_clock::wall())
        })?;
        if !isolated {
            return self.reader_keys(workspace, readers);
        }
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: workspace.to_string(),
            document: document.to_owned(),
        };
        self.scoped_keys(&route, Some(readers))
    }

    /// Publishes each change the store made as this device's write of its document, sealed for
    /// the document's reader set.
    async fn publish_written(
        &self,
        workspace: WorkspaceId,
        app: &str,
        written: Publishing,
    ) -> Result<Vec<AppWrite>, WriteRefusal> {
        let mut writes = Vec::with_capacity(written.len());
        for (write, founding) in written {
            let keys = self.store_write_keys(workspace, app, &write.doc, &write.readers);
            let sealed = match keys.map_err(WriteRefusal::Refused)? {
                SealKeys::Here(keys) => keys,
                SealKeys::Waiting | SealKeys::NoSecretStore => {
                    return Err(WriteRefusal::Refused(
                        "this device holds no keys of the document's readers yet".to_owned(),
                    ));
                }
            };
            let (bytes, founding) = match founding {
                Some(whole) => (whole, true),
                None => (write.change.raw_bytes().to_vec(), false),
            };
            writes.push(
                self.write_resource_read_by(
                    workspace,
                    (app, &write.doc),
                    &write.readers,
                    &bytes,
                    founding,
                    Some(&sealed),
                )
                .await?,
            );
        }
        Ok(writes)
    }
}
