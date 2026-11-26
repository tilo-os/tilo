//! A server that runs a workspace's apps signs their changes as itself, deciding each request as
//! the caller of the call it serves: through its identity socket, as an app's process asks it.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the server and workspace handles a test binds are shared handles it keeps until the test ends"
)]
// The fixtures below are test code outside a `#[test]` function, which clippy does not see as
// one: a failed step is the test failing.
#![allow(
    clippy::expect_used,
    reason = "test fixtures: a failed step is the test failing"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use collab_types::{DocumentMutation, SignedOp, document_artifact_id, document_attestation_id};
use tilo_desktop_model::{Workspace, WorkspaceId};
use tilo_identity::broker::WorkspaceMembership;
use tilo_identity_client::wire::{
    FRAMING, HANDSHAKE_FRAMING, Handshake, PresentationHandshake, Request, Response,
};
use tilo_peer_state::join::Role;
use tilo_user_identity::DeviceCert;

use super::tests::{admit_device, governed};
use crate::apps::caller_signer::{CallerSigner, Callers, VerifiedCaller, editing_unavailable};
use crate::apps::service_workspaces::{block_on, describe};
use crate::workspaces::tests::{governor, test_host};
use crate::workspaces::{SetMemberRoleRequest, Workspaces};

const APP: &str = "so.tilo.notes";
const RESOURCE: &str = "meeting-notes";

/// The calls running now, by invocation, each with its caller.
#[derive(Default)]
struct Running(Mutex<HashMap<[u8; 16], DeviceCert>>);

impl Callers for Running {
    fn caller(&self, _: &WorkspaceId, app: &str, invocation: [u8; 16]) -> Option<VerifiedCaller> {
        let calls = self.0.lock().expect("the calls");
        let cert = calls.get(&invocation).filter(|_| app == APP)?;
        Some(VerifiedCaller {
            device_public_key: cert.device_public_key,
            user_public_key: cert.user_public_key,
        })
    }
}

/// The workspaces the server keeps, as the identity service reads them.
struct Kept(Workspaces, tokio::runtime::Handle);

impl WorkspaceMembership for Kept {
    fn with_workspace(&self, workspace: &WorkspaceId, read: &mut dyn FnMut(&Workspace)) {
        let (workspaces, id) = (self.0.clone(), *workspace);
        if let Some(described) = block_on(&self.1, async move {
            let (name, peer) = workspaces.governed_copy(id).await?;
            describe(id, &name, &peer)
        }) {
            read(&described);
        }
    }
}

/// An app's connection to the identity socket at `socket`, for `workspace`.
struct App(std::os::unix::net::UnixStream);

impl App {
    fn connect(socket: &std::path::Path, workspace: &str) -> Self {
        let mut stream =
            std::os::unix::net::UnixStream::connect(socket).expect("the identity socket");
        let handshake = PresentationHandshake {
            handshake: Handshake {
                workspace_id: workspace.to_owned(),
                app_id: APP.to_owned(),
                surface: None,
                call: None,
            },
            presentation_version: 0,
        };
        let handshake = serde_json::to_vec(&handshake).expect("the handshake encodes");
        HANDSHAKE_FRAMING
            .write(&mut stream, &handshake)
            .expect("the handshake is sent");
        Self(stream)
    }

    fn ask(&mut self, request: &Request) -> Response {
        FRAMING
            .write_value(&mut self.0, request)
            .expect("the request is sent");
        FRAMING
            .read_value(&mut self.0)
            .expect("the answer reads")
            .expect("the service answers")
    }
}

/// An edit of one of the app's resources, unsigned, by `author`.
fn edit(workspace: &str, author: &DeviceCert) -> Vec<u8> {
    let op = SignedOp::new_unsigned(
        DocumentMutation::Snapshot(b"automerge document".to_vec())
            .encode()
            .expect("the edit encodes"),
        document_artifact_id(APP, workspace, RESOURCE),
        collab_types::Ed25519PublicKey::from_bytes(author.device_public_key),
        document_attestation_id(APP, workspace, RESOURCE),
        collab_types::OpKind::Change,
        std::time::SystemTime::now(),
        1,
    );
    postcard::to_stdvec(&op).expect("the edit encodes")
}

fn for_call(invocation: [u8; 16], request: Request) -> Request {
    Request::ForCall {
        invocation,
        request: Box::new(request),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::too_many_lines,
    reason = "one workspace's callers told whole, each asking through the same socket"
)]
async fn a_server_signs_an_apps_change_only_for_a_caller_who_may_make_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(&dir.path().join("server"), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, _, _) = governed(workspaces).await;
    let editor = admit_device(workspaces, &workspace, &owner, 1).await.1;
    let viewer = admit_device(workspaces, &workspace, &owner, 2).await.1;
    workspaces
        .set_member_role(
            &workspace,
            &owner,
            SetMemberRoleRequest {
                request_id: [40; 32],
                user_public_key: viewer.user_public_key,
                role: Role::Viewer,
            },
        )
        .await
        .unwrap();
    let outsider = crate::workspaces::fixtures::device_cert(
        &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
        "outsider",
        tilo_user_identity::DeviceCapabilities::member(),
    )
    .1;

    // The server's own device, kept where its identity is.
    let paths = tilo_device_identity::persist::IdentityPaths::under(dir.path().join("identity"));
    std::fs::create_dir_all(paths.device_privkey.parent().unwrap()).unwrap();
    tilo_device_identity::persist::write_device_key(&paths.device_privkey, &key).unwrap();
    tilo_device_identity::persist::write_device_cert(&paths.device_cert, &owner).unwrap();

    let running = Arc::new(Running::default());
    let runtime = tokio::runtime::Handle::current();
    let socket = dir.path().join("identity.sock");
    let _identity = tilo_identity::broker::spawn_signing(
        socket.clone(),
        Arc::new(Kept(workspaces.clone(), runtime.clone())),
        Arc::new(()),
        Arc::new(()),
        Arc::new(CallerSigner::new(
            workspaces.clone(),
            runtime,
            paths,
            owner.user_public_key,
            Arc::clone(&running) as Arc<dyn Callers>,
        )),
    )
    .unwrap();
    let (by_editor, by_viewer, by_outsider) = (rand::random(), rand::random(), rand::random());
    {
        let mut calls = running.0.lock().unwrap();
        calls.insert(by_editor, editor);
        calls.insert(by_viewer, viewer);
        calls.insert(by_outsider, outsider);
    }
    let unsigned = edit(&workspace, &owner);
    let change = || Request::CollaborationOp {
        workspace_id: workspace.clone(),
        app_id: APP.to_owned(),
        document_id: RESOURCE.to_owned(),
        unsigned_op: unsigned.clone(),
    };
    let keys = || Request::GroupKeys {
        workspace_id: workspace.clone(),
        app_id: APP.to_owned(),
        document_id: RESOURCE.to_owned(),
        known_generation: 0,
    };

    let asks = [
        // Who signs: the server's own device, as itself.
        Request::Metadata,
        // A change made for nobody's call, or for a call that is not running.
        change(),
        for_call(rand::random(), change()),
        // A change made for an editor's call is signed as the server's device.
        for_call(by_editor, change()),
        // A viewer may not change the resource, and someone not in the workspace may not act in
        // it at all.
        for_call(by_viewer, change()),
        for_call(by_outsider, change()),
        // Nor read its keys.
        for_call(by_outsider, keys()),
        keys(),
    ];
    let answers = tokio::task::spawn_blocking({
        let (socket, workspace) = (socket.clone(), workspace.clone());
        move || {
            let mut app = App::connect(&socket, &workspace);
            asks.iter().map(|ask| app.ask(ask)).collect::<Vec<_>>()
        }
    })
    .await
    .unwrap();
    let unavailable = editing_unavailable(APP);
    assert_eq!(
        unavailable,
        "Editing so.tilo.notes on the web isn't available yet. You can read and search here, and \
         edit in the Tilo app."
    );
    let [
        metadata,
        no_call,
        not_running,
        editor,
        viewer,
        outsider,
        outsider_keys,
        no_call_keys,
    ] = answers.try_into().unwrap();
    let Response::Metadata {
        signing_public_key,
        group_public_key,
        ..
    } = metadata
    else {
        panic!("the server did not say who signs: {metadata:?}");
    };
    assert_eq!(signing_public_key, owner.device_public_key);
    assert_eq!(group_public_key, owner.user_public_key);
    for refused in [&no_call, &not_running, &no_call_keys] {
        assert!(
            matches!(refused, Response::Denied(reason) if *reason == unavailable),
            "{refused:?}"
        );
    }
    let Response::Signature(signature) = editor else {
        panic!("the editor's change was not signed: {editor:?}");
    };
    let op: SignedOp = postcard::from_bytes(&unsigned).unwrap();
    ed25519_dalek::Verifier::verify(
        &key.verifying_key(),
        &op.signing_bytes().unwrap(),
        &ed25519_dalek::Signature::from_bytes(&signature.try_into().unwrap()),
    )
    .expect("signed by the server's own device");
    assert!(
        matches!(&viewer, Response::Denied(reason) if reason.contains("denies this document action")),
        "{viewer:?}"
    );
    for refused in [&outsider, &outsider_keys] {
        assert!(
            matches!(refused, Response::Denied(reason) if reason == "the caller may no longer act in this workspace"),
            "{refused:?}"
        );
    }
}
