//! A device's own writes into the documents of the workspaces its service holds.
//!
//! A session's own roles — its layout, the documents it keeps of a workspace's agents — write the
//! workspace's documents without an app or a peer asking: the write is drafted as this device's,
//! signed and sealed under the document's group keys, and committed through the service's one
//! commit path ([`Workspaces::commit_local_document`]), so it is kept, carried and published as
//! any member's write is.

use collab_types::sign_op;
use tilo_peer_state::owner::{AppWrite, WriteRefusal};

use super::Workspaces;

impl Workspaces {
    /// Write `changes` to `app`'s `document` of `workspace` as this device's own: the document's
    /// founding write when `founding` (the whole document, which the workspace must not hold
    /// yet), a change of it otherwise; the whole document when `snapshot` (a founding or a
    /// checkpoint of one the workspace holds), sealed under `sealed` where its documents are
    /// sealed, or under the keys of the document's group this device reads when the writer names
    /// none: a shell writes as the device and never holds the keys itself. Answers the write as this device's apps take a member's: their copies of the
    /// document follow it only once it is handed to them.
    ///
    /// # Errors
    /// [`WriteRefusal::Waiting`] while this device is still taking the workspace in, or what the
    /// write needs is not here yet: the write is made again once it is. Otherwise a founding write
    /// finds the document held already, or the workspace refused the write:
    /// [`WriteRefusal::ChangeRefused`] names the document it refused, the changes it carried and
    /// why, as a server's refusal of a change does.
    pub(crate) async fn own_write(
        &self,
        workspace: &str,
        (app, document): (&str, &str),
        readers: Option<&str>,
        changes: &[u8],
        founding: bool,
        snapshot: bool,
        sealed: Option<&key_group::ContentKeys>,
    ) -> Result<AppWrite, WriteRefusal> {
        self.write_as_device(
            workspace,
            (app, document),
            readers,
            changes,
            (founding, snapshot),
            sealed,
            None,
        )
        .await
    }

    /// [`Self::own_write`], as one part of the action `commit` names when it has one: the commit,
    /// naming this device as its author, travels inside the signed write, so every device takes
    /// the action's parts together.
    pub(in crate::workspaces) async fn write_as_device(
        &self,
        workspace: &str,
        (app, document): (&str, &str),
        readers: Option<&str>,
        changes: &[u8],
        (founding, snapshot): (bool, bool),
        sealed: Option<&key_group::ContentKeys>,
        commit: Option<&collab_types::WorkspaceCommit>,
    ) -> Result<AppWrite, WriteRefusal> {
        let device = self.signing_device();
        let now = tilo_clock::wall();
        let commit = match commit {
            Some(commit) if !commit.covers(app, document) => {
                return Err(WriteRefusal::Refused(format!(
                    "the action's commit names no change of {document} of {app}"
                )));
            }
            Some(commit) => {
                let mut commit = commit.clone();
                commit.author.device = device.device();
                Some(commit)
            }
            None => None,
        };
        // A workspace this device does not hold yet is one it is still joining or opening: the
        // write is made once it holds it, and nobody is told of a refusal.
        let state = crate::timing::said_if_long("opening the workspace", self.workspace(workspace))
            .await
            .map_err(|_| WriteRefusal::Waiting(tilo_peer_state::MissingPrerequisite::Workspace))?;
        let drafting = crate::timing::start();
        let (envelopes, author) = {
            let governed = crate::timing::said_if_long("the workspace's lock", state.lock()).await;
            let picked = match sealed {
                Some(_) => None,
                None => self
                    .sealing_keys(&governed, app, document)
                    .await
                    .map_err(|why| {
                        tracing::debug!(%workspace, app, document, %why, "the write waits for its keys");
                        WriteRefusal::Waiting(tilo_peer_state::MissingPrerequisite::PersistedMls)
                    })?,
            };
            let sealed = sealed.or_else(|| picked.as_ref().map(|keys| &keys.keys));
            tilo_peer_state::owner::draft_local_write(
                &governed.peer(),
                governed.workspace(),
                &device.device(),
                tilo_peer_state::owner::LocalWrite {
                    app,
                    document,
                    readers,
                    changes,
                    founding,
                    snapshot,
                    sealed,
                    commit: commit.as_ref(),
                    now,
                },
            )?
        };
        crate::timing::event("own_write.draft", drafting, changes.len() as u64);
        // A write too large for one frame is committed a segment at a time, in order.
        let mut last = None;
        for mut envelope in envelopes {
            let signing = crate::timing::start();
            sign_op(&mut envelope, &*device.signer)
                .map_err(|error| WriteRefusal::Unsigned(error.to_string()))?;
            crate::timing::event("own_write.sign", signing, 0);
            let committing = crate::timing::start();
            crate::timing::said_if_long(
                "the commit to the workspace",
                self.commit_local_document(workspace, app, document, readers, envelope.clone()),
            )
            .await?;
            crate::timing::event("own_write.commit", committing, 0);
            last = Some(envelope);
        }
        let envelope =
            last.ok_or_else(|| WriteRefusal::Refused("a write that carries no frame".into()))?;
        Ok(AppWrite {
            peer: neighbour_wire::peer_handle(&device.device()),
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            document_id: document.to_owned(),
            author,
            replayed: false,
            envelope,
        })
    }
}

/// A sealed write is followed only where the device reads the groups' keys.
#[cfg(all(test, feature = "apps", feature = "device-keys"))]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::significant_drop_tightening,
        reason = "a test failing is its assertion failing; the service lives until the test ends"
    )]

    use std::sync::Arc;

    use automerge::transaction::{CommitOptions, Transactable as _};
    use automerge::{ReadDoc as _, Value};
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
    use tilo_resource_store::wire::Reply;
    use tilo_user_identity::DeviceCapabilities;

    use super::super::fixtures::{
        TestGroupKeys, device_cert, governor_with, test_host, test_host_holding,
        test_host_in_the_clear,
    };
    use super::super::{MemberDevice, Workspaces};
    use tilo_peer_state::workspace_documents::DocumentRead;

    /// The front `workspace`'s layout stands on, as its document reads now.
    async fn front(workspaces: &Workspaces, workspace: &str) -> Option<String> {
        let read = workspaces
            .own_read(
                workspace,
                (DESKTOP_APP, DESKTOP_DOCUMENT),
                &[],
                64 * 1024 * 1024,
            )
            .await
            .expect("the document reads");
        let DocumentRead::Whole { bytes, .. } = read else {
            panic!("a document this device holds reads whole, not {read:?}");
        };
        let document = automerge::AutoCommit::load(&bytes).expect("the document opens");
        match document
            .get(automerge::ROOT, "front")
            .expect("the document's front")
        {
            Some((Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
            _ => None,
        }
    }

    /// The session's own writes reach the workspace's documents through its service: a write is
    /// read back at once, a later change builds on what it wrote, and a write that founds a
    /// document the workspace holds already is refused.
    #[tokio::test]
    async fn a_devices_own_write_reaches_the_workspaces_documents() {
        let dir = tempfile::tempdir().expect("a state directory");
        let keys = Arc::new(TestGroupKeys::default());
        let root = SigningKey::from_bytes(&[1; 32]);
        let (key, cert) = governor_with(DeviceCapabilities::member());
        let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
        let workspaces = server.workspaces().clone();
        let created = workspaces.create("Layout").await.expect("the workspace");
        let workspace = created.workspace;
        // The service acts as this device, the person's own, as a desktop's does: what it writes
        // is the device's.
        let (member_key, member_cert) = device_cert(&root, "Laptop", DeviceCapabilities::member());
        workspaces
            .enter(&workspace, (&member_cert, &member_key))
            .await
            .expect("the device lists itself");
        workspaces.set_member_device(Arc::new(MemberDevice {
            cert: member_cert,
            signer: Arc::new(member_key),
        }));
        // The document's group keys, as the device's key service hands them out.
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: DESKTOP_APP.into(),
            cluster_id: workspace.clone(),
            document: DESKTOP_DOCUMENT.into(),
        };
        let sealed = workspaces
            .own_route_keys(&route, true)
            .await
            .expect("the device's key service answers")
            .expect("the group opens");

        let mut document = automerge::AutoCommit::new();
        document
            .put(automerge::ROOT, "front", "Main")
            .expect("the layout's front");
        workspaces
            .own_write(
                &workspace,
                (DESKTOP_APP, DESKTOP_DOCUMENT),
                None,
                &document.save(),
                true,
                true,
                Some(&sealed),
            )
            .await
            .expect("the founding write is taken");
        assert_eq!(
            front(&workspaces, &workspace).await.as_deref(),
            Some("Main"),
            "the write is read back at once"
        );

        let heads = document.get_heads();
        document
            .put(automerge::ROOT, "front", "Elsewhere")
            .expect("the layout's front moves");
        // Committed as the desktop commits its own edits: a native change, its signer's.
        document.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Native.change_message()),
        );
        workspaces
            .own_write(
                &workspace,
                (DESKTOP_APP, DESKTOP_DOCUMENT),
                None,
                &document.save_after(&heads),
                false,
                false,
                Some(&sealed),
            )
            .await
            .expect("the change is taken");
        assert_eq!(
            front(&workspaces, &workspace).await.as_deref(),
            Some("Elsewhere"),
            "a change builds on what the device wrote"
        );

        let refused = workspaces
            .own_write(
                &workspace,
                (DESKTOP_APP, DESKTOP_DOCUMENT),
                None,
                &document.save(),
                true,
                true,
                Some(&sealed),
            )
            .await;
        assert!(
            refused.is_err(),
            "a founding write of a document the workspace holds is refused"
        );
        drop(workspaces);
        server.shutdown().await;
    }

    /// The app whose documents the tests below write, as its connection writes them.
    const NOTEBOOK: &str = "so.tilo.test.notebook";

    /// `changes` written to the notebook's record `id` of `workspace`, as the app's connection
    /// writes it: `Err` naming why the service refused it.
    async fn app_write(
        workspaces: &Workspaces,
        workspace: &str,
        id: &str,
        changes: Vec<u8>,
    ) -> Result<(), String> {
        workspaces
            .opened_record_change(workspace, (NOTEBOOK, id), &changes, None, None)
            .await
    }

    /// What the notebook's record `id` of `workspace` reads, as the app's connection reads it:
    /// its changes and heads, and whether it is on its way.
    async fn app_read(workspaces: &Workspaces, workspace: &str, id: &str) -> (Vec<u8>, bool) {
        match workspaces
            .opened_record_changes(workspace, (NOTEBOOK, id), &[], 0)
            .await
            .expect("the read is answered")
        {
            Reply::RecordChanges {
                changes,
                next: None,
                pending,
                ..
            } => (changes.0, pending),
            other => panic!("a read of a small record answers it in one page: {other:?}"),
        }
    }

    /// The title the notebook's record `id` of `workspace` holds, as the app reads it back.
    async fn title(workspaces: &Workspaces, workspace: &str, id: &str) -> String {
        let (changes, _) = app_read(workspaces, workspace, id).await;
        let document = automerge::AutoCommit::load(&changes).expect("the record opens");
        match document.get(automerge::ROOT, "title").expect("its title") {
            Some((Value::Scalar(value), _)) => value.as_str().unwrap_or_default().to_owned(),
            other => panic!("a title, not {other:?}"),
        }
    }

    /// `document`'s title set to `title` in one change, committed with `message`: what the change
    /// says made it.
    fn retitled(document: &automerge::AutoCommit, title: &str, message: Option<String>) -> Vec<u8> {
        let mut document = document.clone();
        let heads = document.get_heads();
        document
            .put(automerge::ROOT, "title", title)
            .expect("a title puts");
        match message {
            Some(message) => document.commit_with(CommitOptions::default().with_message(message)),
            None => document.commit(),
        };
        document.save_after(&heads)
    }

    /// An app writes its document over its connection, and the workspace takes only what every
    /// reader of it takes. A change made for someone the workspace does not grant is refused as it
    /// is written, and so is, `in_the_clear`, a change that says nothing of what made it, which no
    /// device signs in the clear: neither is kept to leave the document unreadable. The document
    /// reads as it was, and the app's own next change is taken.
    async fn a_write_its_readers_refuse_is_refused_and_the_document_stays_readable(
        workspaces: &Workspaces,
        workspace: &str,
        in_the_clear: bool,
    ) {
        let id = uuid::Uuid::new_v4().to_string();
        let native = || tilo_actions::FrameworkChange::Native.change_message();
        let mut founded = automerge::AutoCommit::new();
        founded
            .put(automerge::ROOT, "title", "Plan")
            .expect("a title puts");
        founded.commit_with(CommitOptions::default().with_message(native()));
        app_write(workspaces, workspace, &id, founded.save_after(&[]))
            .await
            .expect("the app's document is founded");
        assert_eq!(title(workspaces, workspace, &id).await, "Plan");

        let stranger = tilo_action_policy::Acting::person(hex::encode([9; 32]));
        let for_stranger = tilo_actions::ActionContext::for_resource(&id, "note", "Plan")
            .with_invoker(tilo_actions::ActionInvoker::admitted(&stranger))
            .change_message();
        let refused = app_write(
            workspaces,
            workspace,
            &id,
            retitled(&founded, "Taken over", for_stranger),
        )
        .await
        .expect_err("a change made for someone the workspace does not grant is refused");
        assert!(
            refused.contains("not granted"),
            "the refusal says why: {refused}"
        );
        assert_eq!(
            title(workspaces, workspace, &id).await,
            "Plan",
            "the document reads as it was"
        );

        if in_the_clear {
            let unnamed = app_write(
                workspaces,
                workspace,
                &id,
                retitled(&founded, "Nameless", None),
            )
            .await
            .expect_err("a change that names nothing that made it is refused, not signed");
            assert!(
                unnamed.contains("names no one who made it"),
                "the refusal says why: {unnamed}"
            );
            assert_eq!(title(workspaces, workspace, &id).await, "Plan");
        }

        app_write(
            workspaces,
            workspace,
            &id,
            retitled(&founded, "Plan, revised", Some(native())),
        )
        .await
        .expect("the app's own change is taken");
        assert_eq!(title(workspaces, workspace, &id).await, "Plan, revised");
    }

    /// [`a_write_its_readers_refuse_is_refused_and_the_document_stays_readable`] in a workspace
    /// whose device seals its documents, which its readers decide only once they open them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_sealed_write_its_readers_refuse_is_refused_as_it_is_written() {
        let dir = tempfile::tempdir().expect("a state directory");
        let keys = Arc::new(TestGroupKeys::default());
        let root = SigningKey::from_bytes(&[1; 32]);
        let (key, cert) = governor_with(DeviceCapabilities::member());
        let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
        let workspaces = server.workspaces().clone();
        let workspace = workspaces
            .create("Notes")
            .await
            .expect("the workspace")
            .workspace;
        let (member_key, member_cert) = device_cert(&root, "Laptop", DeviceCapabilities::member());
        workspaces
            .enter(&workspace, (&member_cert, &member_key))
            .await
            .expect("the member's device lists itself");
        workspaces.set_member_device(Arc::new(MemberDevice {
            cert: member_cert,
            signer: Arc::new(member_key),
        }));
        a_write_its_readers_refuse_is_refused_and_the_document_stays_readable(
            &workspaces,
            &workspace,
            false,
        )
        .await;
        drop(workspaces);
        server.shutdown().await;
    }

    /// The same in a workspace that keeps its documents in the clear, deciding each write as it
    /// takes it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_write_in_the_clear_its_readers_refuse_is_refused_as_it_is_written() {
        let dir = tempfile::tempdir().expect("a state directory");
        let (key, cert) = governor_with(DeviceCapabilities::member());
        let server = test_host_in_the_clear(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces().clone();
        let workspace = workspaces
            .create("Notes")
            .await
            .expect("the workspace")
            .workspace;
        let root = SigningKey::from_bytes(&[1; 32]);
        let (member_key, member_cert) = device_cert(&root, "Laptop", DeviceCapabilities::member());
        workspaces
            .enter(&workspace, (&member_cert, &member_key))
            .await
            .expect("the member's device lists itself");
        workspaces.set_member_device(Arc::new(MemberDevice {
            cert: member_cert,
            signer: Arc::new(member_key),
        }));
        a_write_its_readers_refuse_is_refused_and_the_document_stays_readable(
            &workspaces,
            &workspace,
            true,
        )
        .await;
        drop(workspaces);
        server.shutdown().await;
    }

    /// A window that asks for its documents before this device holds their workspace (one the
    /// session restored at login, while the service still opens it) is told they are on their
    /// way, as a document whose keys have not come yet is, never that the read failed; once the
    /// workspace is held, the read answers from it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_before_the_workspace_is_held_is_answered_on_its_way() {
        let dir = tempfile::tempdir().expect("a state directory");
        let (key, cert) = governor_with(DeviceCapabilities::member());
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces().clone();
        let id = uuid::Uuid::new_v4().to_string();
        let arriving = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            app_read(&workspaces, &arriving, &id).await,
            (Vec::new(), true),
            "a record of a workspace not held yet is on its way"
        );
        let workspace = workspaces
            .create("Notes")
            .await
            .expect("the workspace")
            .workspace;
        assert_eq!(
            app_read(&workspaces, &workspace, &id).await,
            (Vec::new(), false),
            "a held workspace answers what it holds: nothing of this record"
        );
        drop(workspaces);
        server.shutdown().await;
    }
}
