//! A device's own writes into the documents of the workspaces its service holds.
//!
//! A session's own roles — its layout, the documents it keeps of a workspace's agents — write the
//! workspace's documents without an app or a peer asking: the write is drafted as this device's,
//! signed and sealed under the document's group keys, and committed through the service's one
//! commit path ([`Workspaces::commit_local_document`]), so it is kept, carried and published as
//! any member's write is.

use collab_types::{ContentRetention, DocumentMutation, document_artifact_id, sign_op};
use tilo_peer_state::owner::{AppWrite, WriteRefusal};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};

use super::{GovernedWorkspace, SystemTime, Workspaces};

impl GovernedWorkspace {
    /// The write of `changes` to `app`'s `document` of this workspace as `device` makes it: the
    /// whole document when it is founded, a change of it otherwise, sealed under `sealed` (the
    /// keys of the document's group) where the workspace seals its documents. Answers the request
    /// to commit and the author's access in the workspace.
    pub(super) fn draft_document_write(
        &self,
        device: &[u8; 32],
        (app, document, readers): (&str, &str, Option<&str>),
        changes: &[u8],
        founding: bool,
        sealed: Option<&key_group::ContentKeys>,
        now: SystemTime,
    ) -> Result<(OwnerCommitRequest, tilo_action_policy::MemberAccess), WriteRefusal> {
        let peer = self.peer();
        if founding && peer.document(app, document).is_some() {
            return Err(WriteRefusal::Refused(format!(
                "{document} of {app} is in the workspace already"
            )));
        }
        let author = peer
            .permissions(*device, now)
            .map_err(|error| WriteRefusal::Refused(error.to_string()))?;
        let artifact = document_artifact_id(app, self.workspace(), document);
        let frame = if founding {
            DocumentMutation::Snapshot(changes.to_vec())
        } else {
            DocumentMutation::Incremental(changes.to_vec())
        }
        .encode()
        .map_err(|error| WriteRefusal::Refused(error.to_string()))?;
        let mut seal = sealed.map(|keys| {
            move |plaintext: &[u8]| {
                keys.seal_content(artifact, ContentRetention::of_frame(plaintext), plaintext)
                    .map_err(|error| error.to_string())
            }
        });
        let envelope = peer
            .draft_write(
                (app, document),
                readers,
                &frame,
                seal.as_mut()
                    .map(|seal| seal as &mut tilo_peer_state::SealCheckpoint<'_>),
                *device,
                0,
                now,
            )
            .map_err(WriteRefusal::Refused)?;
        let own = peer.own_write(app, document, device);
        drop(peer);
        Ok((
            OwnerCommitRequest {
                workspace: self.workspace().to_owned(),
                operation: CommitOperation::Document {
                    app: app.to_owned(),
                    document: document.to_owned(),
                    envelope: Box::new(envelope),
                    own,
                    readers: readers.map(str::to_owned),
                },
            },
            author,
        ))
    }
}

impl Workspaces {
    /// Write `changes` to `app`'s `document` of `workspace` as this device's own: the document's
    /// founding write when `founding` (the whole document, which the workspace must not hold
    /// yet), a change of it otherwise, sealed under `sealed` where its documents are sealed.
    /// Answers the write as this device's apps take a member's: their copies of the document follow
    /// it only once it is handed to them.
    ///
    /// # Errors
    /// The workspace is not held here, a founding write finds the document held already, or the
    /// workspace refused the write: [`WriteRefusal::ChangeRefused`] names the document it
    /// refused, the changes it carried and why, as a server's refusal of a change does.
    pub(crate) async fn own_write(
        &self,
        workspace: &str,
        (app, document): (&str, &str),
        changes: &[u8],
        founding: bool,
        sealed: Option<&key_group::ContentKeys>,
    ) -> Result<AppWrite, WriteRefusal> {
        let device = self.signing_device();
        let now = tilo_clock::wall();
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| WriteRefusal::Refused(error.to_string()))?;
        let (mut request, author) = {
            let governed = state.lock().await;
            governed.draft_document_write(
                &device.device(),
                (app, document, None),
                changes,
                founding,
                sealed,
                now,
            )?
        };
        let CommitOperation::Document { envelope, own, .. } = &mut request.operation else {
            return Err(WriteRefusal::Refused(
                "a local write is a document write".to_owned(),
            ));
        };
        sign_op(envelope, &*device.signer)
            .map_err(|error| WriteRefusal::Unsigned(error.to_string()))?;
        let (envelope, own) = ((**envelope).clone(), own.clone());
        self.commit_local_document(workspace, request)
            .await
            .map_err(WriteRefusal::Refused)?;
        Ok(AppWrite {
            peer: neighbour_wire::peer_handle(&device.device()),
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            document_id: document.to_owned(),
            author,
            replayed: false,
            envelope,
            own,
        })
    }
}

/// A sealed write is followed only where the device reads the groups' keys.
#[cfg(all(test, feature = "apps", any(feature = "agents", feature = "web")))]
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
                &document.save(),
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
                &document.save_after(&heads),
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
                &document.save(),
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
