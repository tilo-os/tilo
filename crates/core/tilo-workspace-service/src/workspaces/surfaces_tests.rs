//! A host updated from a build that named each app's surface by the hash of its `surface.toml`
//! still lists what that build wrote. Every change names the surface of the app version that made
//! it, and the host keeps each surface its workspaces' changes name in its content store, in the
//! form it was named by: an earlier build kept the `surface.toml` itself there, and the changes it
//! wrote name the hash of those bytes. The update reads them as they are, so nothing written
//! before it disappears from the app's listing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test failing is its assertion failing"
)]

use std::sync::Arc;

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use collab_types::surface::{KindSurface, SurfaceFile};
use collab_types::{CommitNamed as _, DocumentMutation, SignedOp};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_user_identity::DeviceCapabilities;

use super::Workspaces;
use super::fixtures::{admit_view_reader, device_cert, governor_with, test_host};

const APP: &str = "so.tilo.test.older-version";
const KIND: &str = "so.tilo.test.older-version.article";
const RECORDS: usize = 12;

/// The app's surface as both builds ship it. The earlier build names the hash of these bytes;
/// this build names the hash of their canonical JSON form ([`SurfaceFile::kept`]), which is
/// another surface as far as an op is concerned.
const SURFACE: &[u8] = b"# An app's `surface.toml`, as the package ships it.
format = 2

[[resources]]
kind = \"so.tilo.test.older-version.article\"
label = \"Article\"
version = 1
fields = [
  { path = \"title\", type = \"text\" },
]
";

/// How the earlier build decided changes: by the surface a change names, read from the content
/// store `dir` as the `surface.toml` it kept there.
struct EarlierBuild {
    dir: std::path::PathBuf,
}

impl collab_types::SurfaceSource for EarlierBuild {
    fn surface(
        &self,
        _document: &collab_types::WrittenDocument<'_>,
        kind: Option<&str>,
        named: Option<&[u8; 32]>,
    ) -> Option<Arc<dyn collab_types::ReceiptSurface>> {
        let named = named?;
        let bytes = std::fs::read(self.dir.join(hex::encode(named))).ok()?;
        if blake3::hash(&bytes).as_bytes() != named {
            return None;
        }
        let file = SurfaceFile::parse(&bytes).ok()?;
        let resource = match kind {
            Some(kind) => file.resource(kind)?,
            None => file.own_resource()?,
        };
        Some(Arc::new(KindSurface(resource.clone())))
    }
}

/// The article records the listing names now.
async fn articles(workspaces: &Workspaces, workspace: &str) -> Vec<String> {
    workspaces
        .records(workspace)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|(app, _)| app == APP)
        .map(|(_, record)| record)
        .collect()
}

/// The title of `record` of [`APP`], as this host reads the record now.
async fn title(workspaces: &Workspaces, workspace: &str, record: &str) -> Option<String> {
    let bytes = workspaces
        .resource_document(workspace, APP, record)
        .await
        .unwrap()?;
    let document = automerge::AutoCommit::load(&bytes).ok()?;
    document
        .get(automerge::ROOT, "title")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// The write `member` makes of `record`, in the clear, naming `surface`: the version of the app
/// that wrote it, whose bytes the content store keeps.
async fn write_naming(
    workspaces: &Workspaces,
    workspace: &str,
    member: &SigningKey,
    record: &str,
    title: &str,
    surface: [u8; 32],
) {
    let bytes = workspaces
        .resource_document(workspace, APP, record)
        .await
        .unwrap()
        .expect("the record the earlier build wrote");
    let mut document = automerge::AutoCommit::load(&bytes).unwrap();
    document.set_actor(automerge::ActorId::from(
        &member.verifying_key().to_bytes()[..],
    ));
    document.put(automerge::ROOT, "title", title).unwrap();
    // Every change names who made it, and which field of which kind it edits.
    document.commit_edit(KIND, "title", &[1; 32]);
    let change = document
        .get_last_local_change()
        .expect("the change just made")
        .raw_bytes()
        .to_vec();
    let mutation = DocumentMutation::Incremental(change).encode().unwrap();
    let author = collab_types::Ed25519PublicKey::from_bytes(member.verifying_key().to_bytes());
    let mut envelope = SignedOp::new_unsigned(
        mutation,
        collab_types::document_artifact_id(APP, workspace, record),
        author,
        collab_types::document_attestation_id(APP, workspace, record),
        collab_types::OpKind::Change,
        tilo_clock::wall(),
        1,
    )
    .with_surface(Some(surface));
    collab_types::sign_op(&mut envelope, member).unwrap();
    let message = NeighbourMsg::AppDocument {
        peer: tilo_peer_state::peer_handle(author.as_bytes()),
        app_id: APP.into(),
        cluster_id: workspace.into(),
        document_id: record.into(),
        envelope: Box::new(envelope),
        readers: None,
    };
    workspaces
        .absorb(
            workspace,
            member.verifying_key().to_bytes(),
            &postcard::to_stdvec(&message).unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one flow through the build that writes, the update, and what it lists afterwards"
)]
async fn an_update_lists_what_a_build_that_named_surfaces_by_their_file_wrote() {
    // The earlier build: it kept the app's `surface.toml` in the host's content store, and named
    // the hash of those bytes in every change it signed.
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("surfaces");
    std::fs::create_dir_all(&store).unwrap();
    let earlier = *blake3::hash(SURFACE).as_bytes();
    std::fs::write(store.join(hex::encode(earlier)), SURFACE).unwrap();
    tilo_peer_state::decide_with_surfaces(
        collab_types::Surfaces::from_source(Arc::new(EarlierBuild { dir: store.clone() })),
        move |app| (app == APP).then_some(earlier),
    );
    let (key, cert) = governor_with(DeviceCapabilities::member());
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let (member_key, member) = device_cert(
        &SigningKey::from_bytes(&[1; 32]),
        "the earlier build's page",
        DeviceCapabilities::member(),
    );
    admit_view_reader(&workspaces, &workspace, &key, &member).await;
    for at in 0..RECORDS {
        let record = format!("article-{at}");
        workspaces
            .author_workspace_document(&workspace, &cert, (APP, record.as_str()), |document| {
                document
                    .put(automerge::ROOT, "title", format!("Article {at}"))
                    .map_err(|error| error.to_string())
            })
            .await
            .unwrap();
    }
    assert_eq!(
        articles(&workspaces, &workspace).await.len(),
        RECORDS,
        "the build that wrote the articles listed them"
    );
    drop(workspaces);
    server.shutdown().await;

    // The update: this build starts on the state the earlier one kept, and its own hash of the
    // app's surface is not the one the earlier build's changes name.
    collab_types::app_surface::register_package(APP, SURFACE);
    assert_ne!(
        collab_types::app_surface::hash_of(APP),
        Some(earlier),
        "this build names a surface of its own for the app"
    );
    crate::surfaces::decide_receipt(&store);
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let listed = articles(&workspaces, &workspace).await;
    assert_eq!(
        listed.len(),
        RECORDS,
        "nothing the earlier build wrote is gone from the update's listing: {listed:?}"
    );
    for at in 0..RECORDS {
        assert!(
            listed.contains(&format!("article-{at}")),
            "the update lists article-{at}: {listed:?}"
        );
        assert_eq!(
            title(&workspaces, &workspace, &format!("article-{at}"))
                .await
                .as_deref(),
            Some(format!("Article {at}").as_str()),
            "the update reads article-{at} as the earlier build wrote it"
        );
    }
    // A change of the earlier build's still arriving names its surface: the update reads that
    // surface from the content store, in the form the earlier build kept it.
    let written = "Article 0, as the earlier build wrote it";
    write_naming(
        &workspaces,
        &workspace,
        &member_key,
        "article-0",
        written,
        earlier,
    )
    .await;
    assert_eq!(
        title(&workspaces, &workspace, "article-0").await.as_deref(),
        Some(written),
        "the update decides a change naming the surface the earlier build kept"
    );
    drop(workspaces);
    server.shutdown().await;
}
