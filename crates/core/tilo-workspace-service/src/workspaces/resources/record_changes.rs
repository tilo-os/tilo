//! The record an app's process opened, as the device's one store keeps it: a resource of a kind
//! the app's surface declares, whose content document is the record. The process merges the
//! document's changes into its own copy and hands the device the changes it makes, which keep
//! their hashes; each is judged by the verb its label names, as every write to the store is. The
//! process holds no key and signs nothing; the broker decided the caller's read already.

use automerge::{Change as AmChange, ChangeHash};
use collab_types::store::Resource;
use collab_types::store::wire::{Bytes, Command, Reply};
use tilo_resource_store::Stamp;

use super::super::{GovernedWorkspace, Workspaces};

/// The kind `app`'s record `record` is in `store`: `kind` when the route named one the app
/// declares, else whichever of the app's kinds holds it. `None` for a record the store holds
/// none of, or one of another app's kinds.
fn kind_of(
    surfaces: &collab_types::app_surface::Surfaces,
    store: &tilo_resource_store::Store,
    app: &str,
    kind: Option<&str>,
    record: &str,
) -> Option<String> {
    surfaces
        .kinds_of(app)
        .iter()
        .filter(|declared| kind.is_none_or(|kind| kind == declared.as_str()))
        .find(|declared| store.index().get(declared, record).is_some())
        .cloned()
}

/// `action`, an app's write of several of its records, as the store keeps it: each part names
/// the document the record's content is written to, and the device that signs it as its author.
/// The write of `record` (of `kind`) is one of them.
///
/// # Errors
/// The action names another app's record, or one this store holds none of, or none of `record`.
fn stored_action(
    surfaces: &collab_types::app_surface::Surfaces,
    store: &tilo_resource_store::Store,
    (app, record, kind): (&str, &str, &str),
    action: &collab_types::WorkspaceCommit,
    device: [u8; 32],
) -> Result<collab_types::WorkspaceCommit, String> {
    if !action.covers(app, record) {
        return Err(format!(
            "the action the changes of {record} are part of names none of them"
        ));
    }
    let mut stored = action.clone();
    stored.author.device = device;
    for part in &mut stored.parts {
        if part.app != app {
            return Err(format!(
                "an action of {app} writes no record of {}",
                part.app
            ));
        }
        let kind = if part.document == record {
            kind.to_owned()
        } else {
            kind_of(surfaces, store, app, None, &part.document).ok_or_else(|| {
                format!(
                    "the action writes {}, which this workspace holds none of",
                    part.document
                )
            })?
        };
        part.document = store.content_doc(&kind, &part.document).ok_or_else(|| {
            format!(
                "the action writes {}, which this workspace holds none of",
                part.document
            )
        })?;
    }
    Ok(stored)
}

impl Workspaces {
    /// The page from byte `from` of the changes of `app`'s `record` (of `kind`, when the route
    /// named it) in `workspace` past the heads `after` (hex), in the clear, and the heads they
    /// bring it to: none of a record the store holds nothing of. A head the record lacks is one
    /// of the app's own changes it has not handed over yet, and says nothing of what it holds
    /// besides.
    pub(in crate::workspaces) async fn opened_record_changes(
        &self,
        workspace: &str,
        (app, kind, record): (&str, Option<&str>, &str),
        after: &[String],
        from: u64,
    ) -> Result<Reply, String> {
        let after = after
            .iter()
            .map(|head| {
                head.parse::<ChangeHash>()
                    .map_err(|_| format!("{head} names no change of a record"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let nothing = |pending| Reply::RecordChanges {
            changes: Bytes::default(),
            heads: Vec::new(),
            next: None,
            pending,
        };
        // A workspace this device is still opening is on its way: the asker shows it arriving
        // and asks again.
        if self.inner.readings.held(workspace).is_none() {
            return Ok(nothing(true));
        }
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        let mut store = self.store_of(&mut governed);
        let Some(kind) = kind_of(&self.inner.host.surfaces, &store, app, kind, record) else {
            return Ok(nothing(false));
        };
        let Some(document) = store
            .content_document(&kind, record)
            .map_err(|error| error.to_string())?
        else {
            return Ok(nothing(false));
        };
        let known: Vec<_> = after
            .into_iter()
            .filter(|head| document.get_change_meta_by_hash(head).is_some())
            .collect();
        let changes: Vec<u8> = document
            .get_changes(&known)
            .iter()
            .flat_map(|change| change.raw_bytes().iter().copied())
            .collect();
        let heads = document
            .get_heads()
            .iter()
            .map(ToString::to_string)
            .collect();
        drop(store);
        drop(governed);
        Reply::record_page(&changes, heads, from)
            .ok_or_else(|| "the page starts past the record's changes".to_owned())
    }

    /// The ids of `app`'s records `workspace`'s store holds: each resource of a kind the app
    /// declares.
    pub(in crate::workspaces) async fn app_records(
        &self,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<String>, String> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        let store = self.store_of(&mut governed);
        Ok(self
            .inner
            .host
            .surfaces
            .kinds_of(app)
            .iter()
            .flat_map(|kind| store.ids_of(kind).map(str::to_owned).collect::<Vec<_>>())
            .collect())
    }

    /// The workspaces this device holds: what search reads.
    #[must_use]
    pub fn stored_workspaces(&self) -> Vec<String> {
        self.held_workspaces()
    }

    /// Every record `workspace`'s store holds of a kind an app's surface declares, by its app
    /// and id: what search reads of the workspace before anything changes.
    ///
    /// # Errors
    /// The workspace is not held here.
    pub fn stored_records(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let (workspaces, workspace) = (self.clone(), workspace.to_owned());
        crate::device_documents::on_its_own(async move {
            workspaces.stored_records_in(&workspace).await
        })
    }

    async fn stored_records_in(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        let store = self.store_of(&mut governed);
        let mut records = Vec::new();
        for (app, kind) in self.inner.host.surfaces.kinds() {
            records.extend(store.ids_of(&kind).map(|id| (app.clone(), id.to_owned())));
        }
        drop(store);
        Ok(records)
    }

    /// The document `app`'s record `record` of `workspace` is, as the store holds it: what the
    /// app's own reader answers search about. `None` for a record the store holds nothing of.
    ///
    /// # Errors
    /// The workspace is not held here, or the record's document does not read.
    pub fn stored_record(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let workspaces = self.clone();
        let (workspace, app, record) = (workspace.to_owned(), app.to_owned(), record.to_owned());
        crate::device_documents::on_its_own(async move {
            workspaces.stored_record_in(&workspace, &app, &record).await
        })
    }

    async fn stored_record_in(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        let mut store = self.store_of(&mut governed);
        let Some(kind) = kind_of(&self.inner.host.surfaces, &store, app, None, record) else {
            return Ok(None);
        };
        Ok(store
            .content_document(&kind, record)
            .map_err(|error| error.to_string())?
            .map(automerge::Automerge::save))
    }

    /// Write `changes`, the app's own changes of its `record` (of `kind`, when the route named
    /// it) in `workspace`, concatenated, as this device's writes to the record's content: read
    /// only through the confined parser, each refused unless its label names the caller
    /// (`names_caller`), then judged and kept as every write to the store is. The record's
    /// first changes found it: a resource of the route's kind, made by the verb of the first change that names one,
    /// unless `founding_refusal` says why the caller may not found one here.
    ///
    /// `action` is the action the changes are one part of when it wrote other records of the app
    /// too, which come as writes of their own: the store takes the record's changes once every
    /// record's are here, so a refused or lost write of one leaves every record as it was.
    pub(in crate::workspaces) async fn opened_record_change(
        &self,
        workspace: &str,
        (app, kind, record): (&str, Option<&str>, &str),
        (changes, action): (&[u8], Option<&collab_types::WorkspaceCommit>),
        names_caller: &(dyn Fn(&Stamp) -> bool + Sync),
        founding_refusal: Option<&str>,
    ) -> Result<(), String> {
        let parsed: Vec<AmChange> = tilo_automerge_isolate::parse_changes(changes)
            .map_err(|error| format!("the record's changes do not read: {error}"))?;
        if parsed.is_empty() {
            return Ok(());
        }
        for change in &parsed {
            let stamp = Stamp::of(change).unwrap_or_default();
            if !names_caller(&stamp) {
                return Err(format!(
                    "a change of {record} names someone other than the caller who wrote it"
                ));
            }
        }
        let device = self.member_identity()?;
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = std::sync::Arc::clone(&state).lock_owned().await;
        let held = kind_of(
            &self.inner.host.surfaces,
            &self.store_of(&mut governed),
            app,
            kind,
            record,
        );
        let kind = match (held, kind) {
            (Some(held), _) => held,
            (None, None) => return Err(format!("{app} named no kind for a new record")),
            (None, Some(kind)) => {
                // A device that keeps the workspace for its owner admits no new record of its
                // own: the owning device does.
                if let Some(refusal) = founding_refusal
                    && !self.is_member(workspace).await?
                {
                    return Err(refusal.to_owned());
                }
                // Founded under the verb of the first change that names one: a record's derived
                // founding names none.
                let mut stamp = parsed
                    .iter()
                    .filter_map(Stamp::of)
                    .find(|stamp| !stamp.verb.is_empty())
                    .unwrap_or_default();
                stamp.device = hex::encode(device.device());
                stamp.device_name.clone_from(&device.cert.device_label);
                let founding = Command::Put {
                    kind: kind.to_owned(),
                    id: record.to_owned(),
                    resource: Resource {
                        readers: collab_types::EVERY_MEMBER.to_owned(),
                        parent: None,
                        order: None,
                        fields: std::collections::BTreeMap::new(),
                    },
                    stamp,
                };
                let writes =
                    tilo_resource_store::wire::draft(&mut self.store_of(&mut governed), founding)
                        .map_err(|refused| refused.error.to_string())?;
                self.keep_writes(governed, std::sync::Arc::clone(&device), app, writes, None)
                    .await?;
                governed = std::sync::Arc::clone(&state).lock_owned().await;
                kind.to_owned()
            }
        };
        let (writes, action) = {
            let store = self.store_of(&mut governed);
            let writes = store
                .content_writes(&kind, record, parsed)
                .map_err(|error| error.to_string())?;
            let action = match action.filter(|action| action.parts.len() > 1) {
                Some(action) => Some(stored_action(
                    &self.inner.host.surfaces,
                    &store,
                    (app, record, &kind),
                    action,
                    device.device(),
                )?),
                None => None,
            };
            drop(store);
            (writes, action)
        };
        self.keep_writes(governed, device, app, writes, action)
            .await
            .map(drop)
    }

    /// Move `app`'s record `record` out of the workspace `from` into `to`, as `caller` asks:
    /// founded in `to` as `target`, holding what it holds now, then removed from `from`, both
    /// under the move verb. Both sides are judged before either is written: whether the caller
    /// may move the record's kind there, whether this device may write it there, and whether each
    /// store admits its change. A move either side refuses writes nothing.
    ///
    /// # Errors
    /// The record is not here, or either workspace refused its side of the move.
    pub async fn move_record(
        &self,
        (from, to): (&str, &str),
        (app, record, target): (&str, &str, &str),
        caller: &tilo_actions::remote::Caller,
    ) -> Result<(), String> {
        if from == to {
            return Err("a record moves to another workspace than its own".to_owned());
        }
        let device = self.member_identity()?;
        let author = device.device();
        let stamp = Stamp {
            verb: tilo_resource_store::MOVE_VERB.to_owned(),
            invoker: Stamp::by(&caller.acting().by),
            invocation: uuid::Uuid::new_v4().to_string(),
            device: hex::encode(author),
            device_name: device.cert.device_label.clone(),
            ..Stamp::default()
        };
        let source = self
            .workspace(from)
            .await
            .map_err(|error| error.to_string())?;
        let (kind, held, removal) = {
            let mut governed = source.lock().await;
            let mut store = self.store_of(&mut governed);
            let kind = kind_of(&self.inner.host.surfaces, &store, app, None, record)
                .ok_or_else(|| format!("{app} keeps no record {record} in {from}"))?;
            let held = store
                .content_document(&kind, record)
                .map_err(|error| error.to_string())?
                .map(automerge::Automerge::save);
            let removal = Command::Remove {
                kind: kind.clone(),
                id: record.to_owned(),
                stamp: stamp.clone(),
            };
            let removal = tilo_resource_store::wire::draft(&mut store, removal)
                .map_err(|refused| refused.error.to_string())?;
            drop(store);
            self.would_keep(&mut governed, (caller, author), (app, &kind), &removal)
                .map_err(|refused| format!("{from} refuses the record leaving: {refused}"))?;
            (kind, held, removal)
        };
        // What it holds now, written afresh as the move's own change: the history it had stays
        // with the workspace it leaves.
        let mut moved = automerge::AutoCommit::new();
        if let Some(held) = held {
            let held = automerge::AutoCommit::load(&held).map_err(|error| error.to_string())?;
            tilo_resource::evolution::copy_into(&held, &mut moved)
                .map_err(|error| error.to_string())?;
        }
        moved.commit_with(
            automerge::transaction::CommitOptions::default().with_message(stamp.message()),
        );
        let content: Vec<AmChange> = moved.get_changes(&[]).into_iter().collect();

        let destination = self
            .workspace(to)
            .await
            .map_err(|error| error.to_string())?;
        // Both sides admit their change: keep the arrival, then the departure.
        self.keep_arrival(
            std::sync::Arc::clone(&destination).lock_owned().await,
            (caller, author),
            (app, &kind),
            (target, to),
            (stamp, std::sync::Arc::clone(&device)),
        )
        .await?;
        self.keep_filling(
            std::sync::Arc::clone(&destination).lock_owned().await,
            (app, &kind),
            (target, std::sync::Arc::clone(&device)),
            content,
        )
        .await?;
        let departing = std::sync::Arc::clone(&source).lock_owned().await;
        self.keep_writes(departing, device, app, removal, None)
            .await
            .map(drop)
    }

    /// Keep the record `kind` of `app` arriving in `to` as `target`, in `governed`'s
    /// destination: drafted there, then kept as `device`'s write once `to` admits it.
    async fn keep_arrival(
        &self,
        mut governed: tokio::sync::OwnedMutexGuard<GovernedWorkspace>,
        (caller, author): (&tilo_actions::remote::Caller, [u8; 32]),
        (app, kind): (&str, &str),
        (target, to): (&str, &str),
        (stamp, device): (Stamp, std::sync::Arc<crate::workspaces::MemberDevice>),
    ) -> Result<(), String> {
        let founding = Command::Put {
            kind: kind.to_owned(),
            id: target.to_owned(),
            resource: Resource {
                readers: collab_types::EVERY_MEMBER.to_owned(),
                parent: None,
                order: None,
                fields: std::collections::BTreeMap::new(),
            },
            stamp,
        };
        let founding =
            tilo_resource_store::wire::draft(&mut self.store_of(&mut governed), founding)
                .map_err(|refused| refused.error.to_string())?;
        self.would_keep(&mut governed, (caller, author), (app, kind), &founding)
            .map_err(|refused| format!("{to} refuses the record arriving: {refused}"))?;
        self.keep_writes(governed, device, app, founding, None)
            .await
            .map(drop)
    }

    /// Keep `content`, what the record holds now, in `destination` at `target` as `app`'s
    /// write of `device`.
    async fn keep_filling(
        &self,
        mut governed: tokio::sync::OwnedMutexGuard<GovernedWorkspace>,
        (app, kind): (&str, &str),
        (target, device): (&str, std::sync::Arc<crate::workspaces::MemberDevice>),
        content: Vec<AmChange>,
    ) -> Result<(), String> {
        let filled = self
            .store_of(&mut governed)
            .content_writes(kind, target, content)
            .map_err(|error| error.to_string())?;
        self.keep_writes(governed, device, app, filled, None)
            .await
            .map(drop)
    }

    /// Whether `writes` of a move of `app`'s record of `kind`, drafted against `governed`'s
    /// store, would be kept there as the device `author`'s writes for `caller`, judged without
    /// keeping any of them.
    fn would_keep(
        &self,
        governed: &mut GovernedWorkspace,
        (caller, author): (&tilo_actions::remote::Caller, [u8; 32]),
        (app, kind): (&str, &str),
        writes: &[tilo_resource_store::Written],
    ) -> Result<(), String> {
        super::caller_may(
            governed,
            caller,
            app,
            &[(kind.to_owned(), tilo_resource_store::MOVE_VERB.to_owned())],
        )?;
        let now = tilo_clock::wall();
        for write in writes {
            governed.may_start(author, (app, &write.doc), now)?;
            self.store_of(governed)
                .check_write(write, author)
                .map_err(|refusal| format!("{refusal:?}"))?;
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "agents"))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a test failing is its assertion failing"
    )]

    use automerge::transaction::{CommitOptions, Transactable as _};
    use collab_types::store::wire::{Bytes, Command, RECORD_PAGE_BYTES, Reply};
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
    use tilo_actions::remote::Caller;
    use tilo_actions::{Actor, MemberId};
    use tilo_resource_store::Stamp;

    use crate::apps::services::ResourceRequestContext;
    use crate::workspaces::fixtures::{TestHost, governor_with, test_host_holding};
    use crate::workspaces::{OwnWorkspaceOffer, Workspaces};

    const NOTES: &str = "tilo.notes";
    const NOTE: &str = "tilo.notes.note";
    /// The notes app's surface: one kind, whose content a window types into, keeping its items in
    /// a list.
    const SURFACE: &str = "{\"format\":2,\"resources\":[{\"fields\":[{\"path\":\"title\",\"type\":\"string\"},{\"path\":\"items\",\"type\":{\"list\":\"text\"}}],\"kind\":\"tilo.notes.note\",\"label\":\"Note\",\"verbs\":[{\"id\":\"type\",\"kind\":\"undoable\"}],\"version\":1}]}";

    /// `len` characters of text that compresses poorly.
    fn noise(len: usize, seed: u64) -> String {
        const LETTERS: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let Some(letter) = LETTERS.get(usize::try_from(state >> 58).unwrap()) else {
                    unreachable!("six bits name one of the sixty-four letters");
                };
                char::from(*letter)
            })
            .collect()
    }

    /// A workspace this service governs and writes in as the person's device, as a desktop's
    /// does, with the notes app's surface known here.
    async fn kitchen(dir: &std::path::Path) -> (TestHost, String) {
        let (host, workspace, _) = kitchen_kept(dir).await;
        (host, workspace)
    }

    /// The device's key, certificate and the keys of its channels: what it starts again with.
    type Device = (
        ed25519_dalek::SigningKey,
        tilo_user_identity::DeviceCert,
        std::sync::Arc<crate::workspaces::fixtures::TestGroupKeys>,
    );

    /// [`kitchen`], with what the device starts again with ([`started`]).
    async fn kitchen_kept(dir: &std::path::Path) -> (TestHost, String, Device) {
        let (key, cert) = governor_with(tilo_user_identity::DeviceCapabilities::member());
        let device = (key, cert, std::sync::Arc::default());
        let host = started(dir, &device).await;
        host.workspaces()
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap();
        let workspace = room(host.workspaces(), "Kitchen").await;
        (host, workspace, device)
    }

    /// The service of `device`, started on what it keeps in `dir`, writing as the person's
    /// device with the notes app's surface known here.
    async fn started(dir: &std::path::Path, (key, cert, keys): &Device) -> TestHost {
        let host = test_host_holding(dir, key, cert, std::sync::Arc::clone(keys)).await;
        let workspaces = host.workspaces();
        crate::workspaces::keep_surface(&workspaces.inner.host.surfaces, NOTES, SURFACE.as_bytes());
        workspaces.set_member_device(std::sync::Arc::new(crate::workspaces::MemberDevice {
            cert: cert.clone(),
            signer: std::sync::Arc::new(key.clone()),
        }));
        host
    }

    /// A workspace this service founds as `name`, its content keys here before an app writes.
    async fn room(workspaces: &Workspaces, name: &str) -> String {
        let workspace = workspaces.create(name).await.unwrap().workspace;
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: NOTES.into(),
            cluster_id: workspace.clone(),
            document: String::new(),
        };
        workspaces
            .own_route_keys(&route, true)
            .await
            .unwrap()
            .unwrap();
        workspace
    }

    /// This device's own caller in `workspace`.
    fn caller_of(workspaces: &Workspaces, workspace: &str) -> tilo_actions::remote::Caller {
        let device = workspaces.member_identity().unwrap();
        tilo_actions::remote::Caller::authenticated(device.cert.user_public_key, device.device())
            .in_workspace(workspace)
    }

    /// The notes app's window writing `changes` of `record` as this device's person.
    async fn write(
        workspaces: &Workspaces,
        workspace: &str,
        record: &str,
        changes: Vec<u8>,
    ) -> Result<Reply, String> {
        write_part(workspaces, workspace, record, changes, None).await
    }

    /// [`write`], as the part of `action` that writes `record`.
    async fn write_part(
        workspaces: &Workspaces,
        workspace: &str,
        record: &str,
        changes: Vec<u8>,
        action: Option<&collab_types::WorkspaceCommit>,
    ) -> Result<Reply, String> {
        let context = ResourceRequestContext::authenticated(
            NOTES,
            workspace,
            caller_of(workspaces, workspace),
        )
        .on_record(record)
        .of_kind(Some(NOTE.to_owned()));
        workspaces
            .resource_command(
                &context,
                Command::RecordChange {
                    record: Some(record.to_owned()),
                    changes: Bytes(changes),
                    more: false,
                    commit: action.cloned(),
                },
            )
            .await
    }

    /// The record's heads as the store holds it.
    async fn heads(workspaces: &Workspaces, workspace: &str, record: &str) -> Vec<String> {
        match workspaces
            .opened_record_changes(workspace, (NOTES, Some(NOTE), record), &[], 0)
            .await
            .unwrap()
        {
            Reply::RecordChanges { heads, .. } => heads,
            other => panic!("a read answers the record's changes, not {other:?}"),
        }
    }

    /// Commit what `note` holds pending under `stamp`'s label.
    fn commit(note: &mut automerge::AutoCommit, stamp: &Stamp) {
        note.commit_with(CommitOptions::default().with_message(stamp.message()));
    }

    fn typed_by(invoker: &Actor) -> Stamp {
        Stamp {
            verb: Stamp::TYPE_VERB.to_owned(),
            invoker: Stamp::by(invoker),
            invocation: "typing".to_owned(),
            ..Stamp::default()
        }
    }

    /// A window's typing is its caller's: the store keeps a change whose label names the caller,
    /// and refuses one that names anyone else, keeping nothing of it, whether it would found the
    /// record or change one already kept.
    #[tokio::test(flavor = "current_thread")]
    async fn a_change_naming_someone_other_than_its_caller_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (host, workspace) = kitchen(dir.path()).await;
        let workspaces = host.workspaces();
        let me = caller_of(workspaces, &workspace).acting().by.clone();
        let someone_else = Actor::Member(MemberId::person("someone-else"));

        let mut forged = automerge::AutoCommit::new();
        forged.put(automerge::ROOT, "title", "Not mine").unwrap();
        commit(&mut forged, &typed_by(&someone_else));
        let refused = write(workspaces, &workspace, "forged", forged.save()).await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.contains("names someone other than the caller")),
            "{refused:?}"
        );
        assert!(
            heads(workspaces, &workspace, "forged").await.is_empty(),
            "the refused record is not kept"
        );

        let mut note = automerge::AutoCommit::new();
        note.put(automerge::ROOT, "title", "Plan").unwrap();
        commit(&mut note, &typed_by(&me));
        write(workspaces, &workspace, "plan", note.save())
            .await
            .expect("the caller's own typing founds the record");
        let kept = heads(workspaces, &workspace, "plan").await;
        assert!(!kept.is_empty());

        let before = note.get_heads();
        note.put(automerge::ROOT, "title", "Plan B").unwrap();
        commit(&mut note, &typed_by(&someone_else));
        assert!(
            write(workspaces, &workspace, "plan", note.save_after(&before))
                .await
                .is_err(),
            "a kept record takes no change naming someone else either"
        );
        assert_eq!(heads(workspaces, &workspace, "plan").await, kept);
    }

    /// A record's change is judged by the verb its label names: one the app's surface does not
    /// declare for the kind is refused, and nothing of the record is kept.
    #[tokio::test(flavor = "current_thread")]
    async fn a_change_naming_an_undeclared_verb_keeps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (host, workspace) = kitchen(dir.path()).await;
        let workspaces = host.workspaces();
        let me = caller_of(workspaces, &workspace).acting().by.clone();
        let mut note = automerge::AutoCommit::new();
        note.put(automerge::ROOT, "title", "Plan").unwrap();
        commit(
            &mut note,
            &Stamp {
                verb: "publish".to_owned(),
                ..typed_by(&me)
            },
        );
        assert!(
            write(workspaces, &workspace, "plan", note.save())
                .await
                .is_err()
        );
        assert!(heads(workspaces, &workspace, "plan").await.is_empty());
    }

    /// One change larger than a frame is written and read back whole: it is kept as segments,
    /// and the store takes it once all of them are here.
    #[tokio::test(flavor = "current_thread")]
    async fn a_change_larger_than_a_frame_is_written_and_read_back_whole() {
        use automerge::ReadDoc as _;
        let dir = tempfile::tempdir().unwrap();
        let (host, workspace) = kitchen(dir.path()).await;
        let workspaces = host.workspaces();
        let me = caller_of(workspaces, &workspace).acting().by.clone();

        let text = noise(4_500_000, 3);
        let mut note = automerge::AutoCommit::new();
        note.put(automerge::ROOT, "title", text.as_str()).unwrap();
        commit(&mut note, &typed_by(&me));
        let change = note.save();
        assert!(change.len() > collab_types::MAX_DOCUMENT_MUTATION_BYTES);
        write(workspaces, &workspace, "long", change)
            .await
            .expect("the store keeps a change larger than a frame");
        assert_eq!(
            title_in(workspaces, &workspace, "long").await.as_deref(),
            Some(text.as_str()),
            "the store holds all of it"
        );

        let mut joined = Vec::new();
        let mut from = Some(0);
        while let Some(at) = from {
            let Ok(Reply::RecordChanges { changes, next, .. }) = workspaces
                .opened_record_changes(&workspace, (NOTES, Some(NOTE), "long"), &[], at)
                .await
            else {
                panic!("the record's changes");
            };
            joined.extend(changes.0);
            from = next;
        }
        let mut read = automerge::Automerge::new();
        read.load_incremental(&joined)
            .expect("the pages join into the record's changes");
        assert_eq!(
            read.get(automerge::ROOT, "title")
                .unwrap()
                .and_then(|(value, _)| value.into_string().ok())
                .as_deref(),
            Some(text.as_str()),
            "it reads back whole"
        );
    }

    /// A record larger than one frame is written in two changes, the first founding it, and read
    /// back page by page into a copy that holds all of it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_record_larger_than_a_frame_is_written_and_read_in_pages() {
        let dir = tempfile::tempdir().unwrap();
        let (host, workspace) = kitchen(dir.path()).await;
        let workspaces = host.workspaces();
        let me = caller_of(workspaces, &workspace).acting().by.clone();

        let mut note = automerge::AutoCommit::new();
        let mut changes = Vec::new();
        for (field, len, seed) in [("draft", 1_400_000, 1), ("revision", 4_500_000, 2)] {
            let heads = note.get_heads();
            note.put(automerge::ROOT, field, noise(len, seed)).unwrap();
            commit(&mut note, &typed_by(&me));
            changes.push(note.save_after(&heads));
        }
        for change in &changes {
            write(workspaces, &workspace, "plan", change.clone())
                .await
                .expect("the store keeps the app's change");
        }

        let page = |after: Vec<String>, from: u64| {
            let (workspace, workspaces) = (workspace.clone(), &workspaces);
            async move {
                match workspaces
                    .opened_record_changes(&workspace, (NOTES, Some(NOTE), "plan"), &after, from)
                    .await
                {
                    Ok(Reply::RecordChanges {
                        changes,
                        heads,
                        next,
                        ..
                    }) => (changes.0, heads, next),
                    other => panic!("a page of the record's changes, not {other:?}"),
                }
            }
        };
        assert!(
            changes[1].len() > collab_types::MAX_DOCUMENT_MUTATION_BYTES,
            "the second change is larger than one frame"
        );
        let (mut joined, heads, mut next) = page(Vec::new(), 0).await;
        let mut pages = 1;
        while let Some(from) = next {
            assert_eq!(
                u64::try_from(joined.len()).unwrap(),
                from,
                "each page is full but the last, and the next starts where it ends"
            );
            let (more, more_heads, after) = page(Vec::new(), from).await;
            assert!(!more.is_empty());
            assert_eq!(
                more_heads, heads,
                "every page names the whole answer's heads"
            );
            joined.extend(more);
            next = after;
            pages += 1;
        }
        assert_eq!(pages, joined.len().div_ceil(RECORD_PAGE_BYTES));
        assert!(pages >= 3, "{pages} pages");

        let mut copy = automerge::Automerge::new();
        copy.load_incremental(&joined)
            .expect("the pages join into the record's changes");
        let source: Vec<String> = note.get_heads().iter().map(ToString::to_string).collect();
        assert_eq!(heads, source, "the record holds what the app wrote");
        assert_eq!(
            copy.get_heads()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            source,
            "the copy holds all of it"
        );

        assert_eq!(
            page(heads.clone(), 0).await,
            (Vec::new(), heads.clone(), None),
            "nothing past what the asker holds"
        );
        assert!(
            workspaces
                .opened_record_changes(&workspace, (NOTES, Some(NOTE), "plan"), &[], u64::MAX)
                .await
                .is_err(),
            "a page past the end is refused"
        );
    }

    /// A note `record` founded in `workspace`, titled `title` by `by`: the window's copy of it.
    async fn note(
        workspaces: &Workspaces,
        workspace: &str,
        record: &str,
        title: &str,
        by: &Actor,
    ) -> automerge::AutoCommit {
        let mut note = automerge::AutoCommit::new();
        note.put(automerge::ROOT, "title", title).unwrap();
        commit(&mut note, &typed_by(by));
        write(workspaces, workspace, record, note.save())
            .await
            .expect("the note is founded");
        note
    }

    /// `note` titled `title` by `by`: the change, and the heads it leaves the note at.
    fn retitled(
        note: &mut automerge::AutoCommit,
        title: &str,
        by: &Actor,
    ) -> (Vec<u8>, Vec<[u8; 32]>) {
        let before = note.get_heads();
        note.put(automerge::ROOT, "title", title).unwrap();
        commit(note, &typed_by(by));
        let heads = note.get_heads().iter().map(|head| head.0).collect();
        (note.save_after(&before), heads)
    }

    /// One action of the notes app in `workspace` that wrote each record at its heads, as the
    /// app's window commits to it.
    fn action(workspace: &str, parts: [(&str, Vec<[u8; 32]>); 2]) -> collab_types::WorkspaceCommit {
        collab_types::WorkspaceCommit::new(
            uuid::Uuid::parse_str(workspace).unwrap(),
            collab_types::CommitAuthor {
                actor: None,
                device: [0; 32],
            },
            1,
            None,
            None,
            parts
                .into_iter()
                .map(|(record, heads)| collab_types::CommitPart {
                    app: NOTES.to_owned(),
                    document: record.to_owned(),
                    heads,
                })
                .collect(),
        )
    }

    /// An action that retitles two notes changes both once both are here, and neither before:
    /// the first note's change waits for the second's.
    #[tokio::test(flavor = "current_thread")]
    async fn an_action_across_two_records_changes_both_once_both_are_here() {
        let dir = tempfile::tempdir().unwrap();
        let (host, workspace) = kitchen(dir.path()).await;
        let workspaces = host.workspaces();
        let me = caller_of(workspaces, &workspace).acting().by.clone();
        let mut plan = note(workspaces, &workspace, "plan", "Plan", &me).await;
        let mut list = note(workspaces, &workspace, "list", "List", &me).await;
        let (plan_b, plan_heads) = retitled(&mut plan, "Plan B", &me);
        let (list_b, list_heads) = retitled(&mut list, "List B", &me);
        let both = action(&workspace, [("plan", plan_heads), ("list", list_heads)]);

        write_part(workspaces, &workspace, "plan", plan_b, Some(&both))
            .await
            .expect("the store keeps the first record's part");
        assert_eq!(
            title_in(workspaces, &workspace, "plan").await.as_deref(),
            Some("Plan"),
            "the first record waits for the action's other record"
        );
        write_part(workspaces, &workspace, "list", list_b, Some(&both))
            .await
            .expect("the store keeps the second record's part");
        assert_eq!(
            title_in(workspaces, &workspace, "plan").await.as_deref(),
            Some("Plan B")
        );
        assert_eq!(
            title_in(workspaces, &workspace, "list").await.as_deref(),
            Some("List B")
        );

        // A change of one record alone after it, an action of one part: kept and read as any,
        // where the note was founded as one action with where its store places it.
        let (list_c, _) = retitled(&mut list, "List C", &me);
        write_part(workspaces, &workspace, "list", list_c, None)
            .await
            .expect("the store keeps a change of one record");
        assert_eq!(
            title_in(workspaces, &workspace, "list").await.as_deref(),
            Some("List C")
        );
    }

    /// An action whose write of one record is refused changes neither record: the other
    /// record's change, kept first, never reaches the store.
    #[tokio::test(flavor = "current_thread")]
    async fn an_action_across_two_records_one_refused_changes_neither() {
        let dir = tempfile::tempdir().unwrap();
        let (host, workspace) = kitchen(dir.path()).await;
        let workspaces = host.workspaces();
        let me = caller_of(workspaces, &workspace).acting().by.clone();
        let someone_else = Actor::Member(MemberId::person("someone-else"));
        let mut plan = note(workspaces, &workspace, "plan", "Plan", &me).await;
        let mut list = note(workspaces, &workspace, "list", "List", &me).await;
        let (plan_heads, list_heads) = (
            heads(workspaces, &workspace, "plan").await,
            heads(workspaces, &workspace, "list").await,
        );
        let (plan_b, plan_at) = retitled(&mut plan, "Plan B", &me);
        // Named as someone else's: the store refuses it.
        let (list_b, list_at) = retitled(&mut list, "List B", &someone_else);
        let both = action(&workspace, [("plan", plan_at), ("list", list_at)]);

        write_part(workspaces, &workspace, "plan", plan_b, Some(&both))
            .await
            .expect("the store keeps the first record's part");
        let refused = write_part(workspaces, &workspace, "list", list_b, Some(&both)).await;
        assert!(refused.is_err(), "{refused:?}");
        assert_eq!(heads(workspaces, &workspace, "plan").await, plan_heads);
        assert_eq!(heads(workspaces, &workspace, "list").await, list_heads);
        assert_eq!(
            title_in(workspaces, &workspace, "plan").await.as_deref(),
            Some("Plan")
        );
        assert_eq!(
            title_in(workspaces, &workspace, "list").await.as_deref(),
            Some("List")
        );
    }

    /// A device that stops between an action's two writes holds neither when it starts again,
    /// and one that stops after both holds both: the first write it kept waits for the second
    /// again as the device replays what it kept.
    #[tokio::test(flavor = "current_thread")]
    async fn a_stop_between_an_actions_two_records_leaves_neither_and_after_both_leaves_both() {
        let dir = tempfile::tempdir().unwrap();
        let (host, workspace, device) = kitchen_kept(dir.path()).await;
        let workspaces = host.workspaces().clone();
        let me = caller_of(&workspaces, &workspace).acting().by.clone();
        let mut plan = note(&workspaces, &workspace, "plan", "Plan", &me).await;
        let mut list = note(&workspaces, &workspace, "list", "List", &me).await;
        let mut shop = note(&workspaces, &workspace, "shop", "Shop", &me).await;
        let mut chores = note(&workspaces, &workspace, "chores", "Chores", &me).await;

        // One action stops after its first write.
        let (plan_b, plan_at) = retitled(&mut plan, "Plan B", &me);
        let (_, list_at) = retitled(&mut list, "List B", &me);
        let cut = action(&workspace, [("plan", plan_at), ("list", list_at)]);
        write_part(&workspaces, &workspace, "plan", plan_b, Some(&cut))
            .await
            .unwrap();
        // Another makes both.
        let (shop_b, shop_at) = retitled(&mut shop, "Shop B", &me);
        let (chores_b, chores_at) = retitled(&mut chores, "Chores B", &me);
        let whole = action(&workspace, [("shop", shop_at), ("chores", chores_at)]);
        write_part(&workspaces, &workspace, "shop", shop_b, Some(&whole))
            .await
            .unwrap();
        write_part(&workspaces, &workspace, "chores", chores_b, Some(&whole))
            .await
            .unwrap();
        drop(workspaces);
        host.shutdown().await;

        let host = started(dir.path(), &device).await;
        let workspaces = host.workspaces();
        for (record, title) in [
            ("plan", "Plan"),
            ("list", "List"),
            ("shop", "Shop B"),
            ("chores", "Chores B"),
        ] {
            assert_eq!(
                title_in(workspaces, &workspace, record).await.as_deref(),
                Some(title),
                "{record} after the device started again"
            );
        }
    }

    /// What `record` of the notes app is titled in `workspace`'s store; `None` when the store
    /// keeps no such record.
    async fn title_in(workspaces: &Workspaces, workspace: &str, record: &str) -> Option<String> {
        use automerge::ReadDoc as _;
        let state = workspaces.workspace(workspace).await.unwrap();
        let mut governed = state.lock().await;
        let mut store = workspaces.store_of(&mut governed);
        store.index().get(NOTE, record)?;
        let doc = store.content_document(NOTE, record).unwrap()?;
        let title = doc
            .get(automerge::ROOT, "title")
            .unwrap()
            .and_then(|(value, _)| value.into_string().ok());
        drop(store);
        title
    }

    /// The person's agent `scout`, as it calls from `workspace`: its certificate chain says
    /// whose agent it is.
    fn scout(workspaces: &Workspaces, workspace: &str) -> Caller {
        let person = workspaces.member_identity().unwrap().cert.user_public_key;
        Caller::delegated_agent("scout", person, [7; 32])
            .in_workspace(workspace)
            .on_roster(1, person)
    }

    /// `workspace` answers what the scout does there with `answer`, as its person granted.
    async fn grant_scout(workspaces: &Workspaces, workspace: &str, answer: Answer) {
        let person = hex::encode(workspaces.member_identity().unwrap().cert.user_public_key);
        crate::workspaces::fixtures::record_grants(
            workspaces,
            workspace,
            &[Grant::new(
                "scout",
                Grantee::agent("scout", person.as_str()),
                TreePath::workspace(workspace),
                KindSet::READ_UNDOABLE,
                answer,
                Grantee::person(person),
            )],
        )
        .await;
    }

    /// Kitchen, holding a note titled "Plan", and Garage, both this device's.
    async fn two_rooms(dir: &std::path::Path) -> (TestHost, String, String) {
        let (host, kitchen_id) = kitchen(dir).await;
        let workspaces = host.workspaces();
        let garage = room(workspaces, "Garage").await;
        let me = caller_of(workspaces, &kitchen_id).acting().by.clone();
        let mut note = automerge::AutoCommit::new();
        note.put(automerge::ROOT, "title", "Plan").unwrap();
        commit(&mut note, &typed_by(&me));
        write(workspaces, &kitchen_id, "plan", note.save())
            .await
            .expect("the caller's own typing founds the record");
        (host, kitchen_id, garage)
    }

    /// A person moving a note between their workspaces: it arrives holding what it held, and
    /// leaves the workspace it was in.
    #[tokio::test(flavor = "current_thread")]
    async fn a_moved_record_arrives_whole_and_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let (host, kitchen_id, garage) = two_rooms(dir.path()).await;
        let workspaces = host.workspaces();
        let me = caller_of(workspaces, &kitchen_id);

        workspaces
            .move_record((&kitchen_id, &garage), (NOTES, "plan", "plan"), &me)
            .await
            .expect("the move is made");
        assert_eq!(
            title_in(workspaces, &garage, "plan").await.as_deref(),
            Some("Plan")
        );
        assert_eq!(title_in(workspaces, &kitchen_id, "plan").await, None);
    }

    /// A move the workspace it arrives in refuses (the person's agent may not move notes there)
    /// writes nothing in either.
    #[tokio::test(flavor = "current_thread")]
    async fn a_move_its_destination_refuses_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (host, kitchen_id, garage) = two_rooms(dir.path()).await;
        let workspaces = host.workspaces();
        let scout = scout(workspaces, &kitchen_id);
        grant_scout(workspaces, &kitchen_id, Answer::Allow).await;
        grant_scout(workspaces, &garage, Answer::Deny).await;

        let refused = workspaces
            .move_record((&kitchen_id, &garage), (NOTES, "plan", "plan"), &scout)
            .await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.contains("refuses the record arriving")),
            "{refused:?}"
        );
        assert_eq!(
            title_in(workspaces, &kitchen_id, "plan").await.as_deref(),
            Some("Plan")
        );
        assert_eq!(title_in(workspaces, &garage, "plan").await, None);
    }

    /// A move the workspace it leaves refuses (the person's agent may not move notes out of it)
    /// writes nothing in either.
    #[tokio::test(flavor = "current_thread")]
    async fn a_move_its_source_refuses_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (host, kitchen_id, garage) = two_rooms(dir.path()).await;
        let workspaces = host.workspaces();
        let scout = scout(workspaces, &kitchen_id);
        grant_scout(workspaces, &kitchen_id, Answer::Deny).await;
        grant_scout(workspaces, &garage, Answer::Allow).await;

        let refused = workspaces
            .move_record((&kitchen_id, &garage), (NOTES, "plan", "plan"), &scout)
            .await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.contains("refuses the record leaving")),
            "{refused:?}"
        );
        assert_eq!(
            title_in(workspaces, &kitchen_id, "plan").await.as_deref(),
            Some("Plan")
        );
        assert_eq!(title_in(workspaces, &garage, "plan").await, None);
    }
    /// The items `record` of the notes app keeps in `workspace`'s store, in order.
    async fn items_in(workspaces: &Workspaces, workspace: &str, record: &str) -> Vec<String> {
        use automerge::ReadDoc as _;
        let state = workspaces.workspace(workspace).await.unwrap();
        let mut governed = state.lock().await;
        let mut store = workspaces.store_of(&mut governed);
        let Some(doc) = store.content_document(NOTE, record).unwrap() else {
            return Vec::new();
        };
        let Some((_, items)) = doc.get(automerge::ROOT, "items").unwrap() else {
            return Vec::new();
        };
        let kept = (0..doc.length(&items))
            .filter_map(|at| doc.get(&items, at).unwrap())
            .filter_map(|(value, _)| value.into_string().ok())
            .collect();
        drop(store);
        kept
    }

    /// The founding every holder derives of the note `record`: its `items` list.
    fn derived(record: &str) -> automerge::Change {
        collab_types::document::founding::structure(
            NOTE,
            record,
            &[("items".to_owned(), automerge::ObjType::List)],
        )
        .expect("a note keeps its items in a list")
    }

    /// A device's first item of `record`, written on the derived founding as `writer` types it.
    fn first_item(record: &str, writer: &str, item: &str, by: &Actor) -> Vec<u8> {
        use automerge::ReadDoc as _;
        use tilo_automerge::ApplyIncoming as _;
        let mut note =
            automerge::AutoCommit::new().with_actor(automerge::ActorId::from(writer.as_bytes()));
        note.apply_incoming([derived(record)]).unwrap();
        let (_, items) = note.get(automerge::ROOT, "items").unwrap().unwrap();
        note.insert(&items, 0, item).unwrap();
        commit(&mut note, &typed_by(by));
        note.save()
    }

    /// Two devices making the same note before they sync each found it on the one derived
    /// founding, so the first item each adds lands in the same list and both are kept.
    #[tokio::test(flavor = "current_thread")]
    async fn two_devices_founding_one_record_at_once_both_keep_their_first_items() {
        let dir = tempfile::tempdir().unwrap();
        let (host, workspace) = kitchen(dir.path()).await;
        let workspaces = host.workspaces();
        let me = caller_of(workspaces, &workspace).acting().by.clone();

        let laptop = first_item("groceries", "laptop", "Milk", &me);
        let phone = first_item("groceries", "phone", "Bread", &me);
        write(workspaces, &workspace, "groceries", laptop)
            .await
            .expect("the laptop founds the note");
        write(workspaces, &workspace, "groceries", phone)
            .await
            .expect("the phone's founding is the one already kept");

        let mut items = items_in(workspaces, &workspace, "groceries").await;
        items.sort();
        assert_eq!(items, ["Bread", "Milk"]);
    }

    /// A founding under the derived actor that holds more than its kind's containers is not the
    /// one every holder makes: the store refuses it and keeps nothing of the record.
    #[tokio::test(flavor = "current_thread")]
    async fn a_forged_founding_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (host, workspace) = kitchen(dir.path()).await;
        let workspaces = host.workspaces();

        let mut forged = automerge::AutoCommit::new()
            .with_actor(tilo_resource::genesis::actor("forged", NOTE, ""));
        forged
            .put_object(automerge::ROOT, "items", automerge::ObjType::List)
            .unwrap();
        forged.put(automerge::ROOT, "title", "Planted").unwrap();
        forged.commit_with(
            CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Genesis.change_message())
                .with_time(0),
        );
        // The window's own typing follows it, as a real founding's does.
        let me = caller_of(workspaces, &workspace).acting().by.clone();
        let mut typed = forged
            .fork()
            .with_actor(automerge::ActorId::from(b"window".as_slice()));
        typed.put(automerge::ROOT, "title", "Mine").unwrap();
        commit(&mut typed, &typed_by(&me));
        let refused = write(workspaces, &workspace, "forged", typed.save()).await;
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.contains("not the one its kind derives")),
            "{refused:?}"
        );
        assert!(heads(workspaces, &workspace, "forged").await.is_empty());
    }
    /// The notes app's own reader: what its binary answers search about a note, a title it
    /// computes from what the note holds.
    struct NoteReader;

    impl crate::search::records::RecordReader for NoteReader {
        fn entries(
            &self,
            _: &str,
            doc: &[u8],
        ) -> Result<Vec<tilo_resource::search_index::IndexEntry>, crate::search::records::Unread>
        {
            use automerge::ReadDoc as _;
            let doc = automerge::AutoCommit::load(doc)
                .map_err(|error| crate::search::records::Unread::Refused(error.to_string()))?;
            let title = doc
                .get(automerge::ROOT, "title")
                .ok()
                .flatten()
                .and_then(|(value, _)| value.into_string().ok())
                .unwrap_or_default();
            Ok(vec![tilo_resource::search_index::IndexEntry {
                key: "note".to_owned(),
                resource: "note".to_owned(),
                title: format!("Note: {title}"),
                ..tilo_resource::search_index::IndexEntry::default()
            }])
        }
    }

    /// The workspaces' stores as the device's search reads them: every app's records, none of
    /// them a tilolang app's.
    struct Stores(Workspaces);

    impl crate::search::records::StoreReader for Stores {
        fn workspaces(&self) -> Vec<String> {
            self.0.stored_workspaces()
        }

        fn records(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
            self.0.stored_records(workspace)
        }

        fn record(
            &self,
            workspace: &str,
            app: &str,
            record: &str,
        ) -> Result<Option<Vec<u8>>, String> {
            self.0.stored_record(workspace, app, record)
        }

        fn reads(&self, _: &str) -> bool {
            false
        }

        fn entries(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Vec<tilo_resource::search_index::IndexEntry>, String> {
            Ok(Vec::new())
        }
    }

    /// What a person searching for `query` finds, once the index answers `expected`.
    async fn found_once(
        search: &crate::search::IndexSearch,
        query: &str,
        expected: &[&str],
    ) -> Vec<String> {
        let started = std::time::Instant::now();
        loop {
            let found: Vec<String> = search
                .found(
                    &tilo_actions::remote::Caller::local_person("ada"),
                    query,
                    &tilo_actions::remote::Within::default(),
                )
                .into_iter()
                .map(|found| found.label)
                .collect();
            if found == expected || started.elapsed() > std::time::Duration::from_secs(20) {
                return found;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// A Rust app's record is found by the title its own reader computes from what the store
    /// holds, once a window's write lands; renamed, it is found by its new title alone.
    #[tokio::test(flavor = "current_thread")]
    async fn a_record_is_found_by_its_computed_title_after_a_write_and_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let (host, workspace) = kitchen(dir.path()).await;
        let workspaces = host.workspaces();
        let me = caller_of(workspaces, &workspace).acting().by.clone();
        let content = std::sync::Arc::new(crate::search::AppContentIndex::in_memory());
        let records = crate::search::records::RecordIndex::start(
            std::sync::Arc::new(NoteReader),
            std::sync::Arc::new(Stores(workspaces.clone())),
            std::sync::Arc::clone(&content),
        );
        let search = crate::search::IndexSearch::new(content, crate::search::deciding(|_, _| true));
        let mut writes = workspaces.document_writes();
        // Search is told of every document a write lands in, as the device's daemon tells it.
        let mut follow = || {
            let mut told = 0;
            while let Ok(write) = writes.try_recv() {
                records.changed(&write.workspace, &write.app, &write.record);
                told += 1;
            }
            assert!(told > 0, "the write is told to search");
        };

        let mut note = automerge::AutoCommit::new();
        note.put(automerge::ROOT, "title", "Groceries").unwrap();
        commit(&mut note, &typed_by(&me));
        write(workspaces, &workspace, "plan", note.save())
            .await
            .expect("the window's typing founds the note");
        follow();
        assert_eq!(
            found_once(&search, "groceries", &["Note: Groceries"]).await,
            ["Note: Groceries"]
        );

        let before = note.get_heads();
        note.put(automerge::ROOT, "title", "Hardware").unwrap();
        commit(&mut note, &typed_by(&me));
        write(workspaces, &workspace, "plan", note.save_after(&before))
            .await
            .expect("the rename is kept");
        follow();
        assert_eq!(
            found_once(&search, "hardware", &["Note: Hardware"]).await,
            ["Note: Hardware"]
        );
        assert!(
            found_once(&search, "groceries", &[]).await.is_empty(),
            "the old title is gone from search"
        );
    }
}
