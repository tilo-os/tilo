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
use tilo_user_identity::DeviceCapabilities;

use super::Workspaces;
use super::fixtures::{TestGroupKeys, governor_with, test_host_holding};

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
    // The device's channel keys are its own and stay with it across a restart, as a device
    // keeps them: the update reads the records' channel with them.
    let keys = Arc::new(TestGroupKeys::default());
    let server = test_host_holding(dir.path(), &key, &cert, Arc::clone(&keys)).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    // The earlier build's workspace ran the app at its version: the changes it wrote name that
    // version's surface, and the workspace lets its members write the app's records.
    let release = tilo_peer_state::workspace_membership::AppRelease {
        app: APP.into(),
        version: "1.0.0".into(),
        surface: hex::encode(earlier),
        package: "00".repeat(32),
        publisher: "00".repeat(32),
        file: None,
        dev: true,
        pinned: false,
        payload: tilo_peer_state::workspace_membership::AppPayload::Tapp,
    };
    workspaces
        .commit_own_membership(
            &workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::InstallApp(release),
            None,
        )
        .await
        .unwrap();
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: APP.into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    assert!(
        workspaces
            .own_route_keys(&route, true)
            .await
            .expect("the workspace answers for its keys")
            .is_ok(),
        "the workspace's channel keys are here"
    );
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
    // The update's start keeps the surfaces its content store holds; the process it runs as the
    // earlier build's device names that build's surface in what it writes.
    crate::surfaces::decide_receipt(&store);
    tilo_peer_state::decide_with_surfaces(
        collab_types::Surfaces::from_source(Arc::new(collab_types::surface::ContentSurfaces::new(
            |record, hash| crate::surfaces::kept(record.app, hash),
        ))),
        move |app| (app == APP).then_some(earlier),
    );
    let server = test_host_holding(dir.path(), &key, &cert, keys).await;
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
    // The update decides against the surface the earlier build kept, in the form it kept it:
    // the source every change naming a surface is resolved through reads those bytes.
    let source = collab_types::surface::ContentSurfaces::new(|record, hash| {
        crate::surfaces::kept(record.app, hash)
    });
    let written = collab_types::WrittenDocument {
        workspace: &workspace,
        app: APP,
        document: "article-0",
    };
    assert!(
        collab_types::SurfaceSource::surface(&source, &written, None, Some(&earlier)).is_some(),
        "the update reads the surface the earlier build kept"
    );
    drop(workspaces);
    server.shutdown().await;
}
