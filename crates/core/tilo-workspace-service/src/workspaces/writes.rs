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
use super::wanted_keys::{KeyWaits, Unkept};

/// How long a phase of a write may wait before it is said while it still waits.
const WAITED_LONG: std::time::Duration = std::time::Duration::from_secs(10);

/// Run `work`, a phase of a write, saying at warn level once it has waited [`WAITED_LONG`] for
/// `phase` and still waits: a write that never ends names the phase it waits in before whoever
/// gives up on it does.
pub(crate) async fn said_if_long<T>(
    phase: &'static str,
    work: impl std::future::Future<Output = T>,
) -> T {
    let mut work = std::pin::pin!(work);
    let waited = std::pin::pin!(tilo_rt::sleep(WAITED_LONG));
    match futures::future::select(work.as_mut(), waited).await {
        futures::future::Either::Left((done, _)) => done,
        futures::future::Either::Right(((), _)) => {
            tracing::warn!(
                phase,
                waited_s = WAITED_LONG.as_secs(),
                "a write still waits"
            );
            work.await
        }
    }
}

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
    pub async fn own_write(
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
        let state = said_if_long("opening the workspace", self.workspace(workspace))
            .await
            .map_err(|_| WriteRefusal::Waiting(tilo_peer_state::MissingPrerequisite::Workspace))?;
        let waiting = |why: String| {
            tracing::debug!(%workspace, app, document, %why, "the write waits for its keys");
            WriteRefusal::Waiting(tilo_peer_state::MissingPrerequisite::PersistedMls)
        };
        // The keys are looked up under the workspace's lock; keys this device holds none of yet
        // are waited for with the lock let go, and the write is drafted under the next hold.
        let mut waits = KeyWaits::default();
        let (envelopes, author) = loop {
            let hosted = said_if_long("the workspace's lock", state.lock()).await;
            let picked = match sealed {
                Some(_) => None,
                None => match self.sealing_keys(&hosted, app, document).await {
                    Ok(picked) => picked,
                    Err(Unkept::Refused(why)) => return Err(waiting(why)),
                    Err(unkept @ Unkept::Keys(_)) => {
                        drop(hosted);
                        waits.wait(unkept).await.map_err(waiting)?;
                        continue;
                    }
                },
            };
            let sealed = sealed.or_else(|| picked.as_ref().map(|keys| &keys.keys));
            break tilo_peer_state::owner::draft_local_write(
                &hosted.peer(),
                hosted.workspace(),
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
            )?;
        };
        // A write too large for one frame is committed a segment at a time, in order.
        let mut last = None;
        for mut envelope in envelopes {
            sign_op(&mut envelope, &*device.signer)
                .map_err(|error| WriteRefusal::Unsigned(error.to_string()))?;
            said_if_long(
                "the commit to the workspace",
                Box::pin(self.commit_local_document(
                    workspace,
                    app,
                    document,
                    readers,
                    envelope.clone(),
                )),
            )
            .await?;
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

    use automerge::transaction::Transactable as _;
    use automerge::{ReadDoc as _, Value};
    use collab_types::store::wire::Reply;
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
    use tilo_user_identity::DeviceCapabilities;

    use super::super::fixtures::{
        TestGroupKeys, device_cert, hosting_device_with, test_host, test_host_holding,
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
        let (key, cert) = hosting_device_with(DeviceCapabilities::member());
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
        // Committed as the device's own edit, naming the person it is for: a change that names
        // no one is refused.
        document.commit_with(
            automerge::transaction::CommitOptions::default().with_message(
                crate::workspaces::authored::own_edit(&workspaces.signing_device().cert),
            ),
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

    /// The app whose record the test below reads, as its connection reads it.
    const NOTEBOOK: &str = "so.tilo.test.notebook";

    /// What the notebook's record `id` of `workspace` reads, as the app's connection reads it:
    /// its changes, and whether it is on its way.
    async fn app_read(workspaces: &Workspaces, workspace: &str, id: &str) -> (Vec<u8>, bool) {
        match workspaces
            .opened_record_changes(workspace, (NOTEBOOK, None, id), &[], 0)
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

    /// A host that takes part in no workspace's channels has no device to found a workspace on:
    /// asked to make one, it refuses, and says why in words its person reads.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_host_with_no_device_founds_no_workspace() {
        let dir = tempfile::tempdir().expect("a state directory");
        let (key, cert) = hosting_device_with(DeviceCapabilities::member());
        let server = test_host_in_the_clear(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces().clone();
        let refused = workspaces
            .create("Notes")
            .await
            .expect_err("no device founds it");
        let refusal = tilo_server_protocol::workspaces::AdminRefusal::from_reason(&refused);
        assert_eq!(
            refusal,
            Some(tilo_server_protocol::workspaces::AdminRefusal::NoDeviceToFound),
            "{refused}"
        );
        assert!(
            refusal.unwrap().to_string().contains("device"),
            "the person is told a device founds it"
        );
        assert!(workspaces.list().await.is_empty(), "nothing was made");
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
        let (key, cert) = hosting_device_with(DeviceCapabilities::member());
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
