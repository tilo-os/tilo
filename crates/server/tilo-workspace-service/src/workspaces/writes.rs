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
        let device = self.signing_device();
        let now = tilo_clock::wall();
        // A workspace this device does not hold yet is one it is still joining or opening: the
        // write is made once it holds it, and nobody is told of a refusal.
        let state = crate::timing::said_if_long("opening the workspace", self.workspace(workspace))
            .await
            .map_err(|_| WriteRefusal::Waiting(tilo_peer_state::MissingPrerequisite::Workspace))?;
        let drafting = crate::timing::start();
        let (mut envelope, author) = {
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
                    now,
                },
            )?
        };
        crate::timing::event("own_write.draft", drafting, changes.len() as u64);
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
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
    use tilo_user_identity::DeviceCapabilities;

    use super::super::fixtures::{
        TestGroupKeys, device_cert, enter_key, governor_with, test_host_holding,
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
        enter_key(&workspaces, &workspace, &member_cert).await;
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
}
