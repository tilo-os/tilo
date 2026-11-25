//! A tilolang app the build carries writes its record on the host that mounted the language.
//!
//! The app's surface names the workspace record the system keeps for it beside its own kinds,
//! as Chat's names the workspace's emoji: the system declares that record from its type as the
//! service starts. The package's other kinds must still be declared, or the app's root is never
//! made and its record never read. No test declares the app's surface here: the mount does, as
//! the daemon's program does.

use tilo_actions::MemberId;
use tilolang_workspace::records::{built_in, member, with_tl_record};

/// Chat, whose app keeps the workspace's emoji (`collab_types::WORKSPACE_DOCUMENTS`).
const APP: &str = ui::apps::CHAT;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_app_that_names_the_workspaces_emoji_still_has_its_own_kinds_declared() {
    let device = super::Device::start("Chat", None).await;
    let workspaces = device.workspaces();
    let workspace = device.workspace.clone();
    let surface = built_in(APP)
        .unwrap()
        .expect("Chat is built into this build")
        .surface(APP)
        .expect("Chat declares its surface");
    assert!(
        collab_types::surface::SurfaceFile::compiled(&surface)
            .expect("the built-in surface is the compiled form")
            .resources
            .iter()
            .any(|resource| resource.kind == "tilo.workspace.emoji"),
        "Chat's surface names the workspace's emoji beside its own kinds"
    );
    // The version the workspace runs names Chat's surface by its hash: the record's writes name
    // it, and one nobody installed is held rather than judged.
    super::govern(
        workspaces,
        &workspace,
        &device.cert,
        "install-app",
        serde_json::json!({
            "app": APP,
            "version": "1.0.0",
            "surface": blake3::hash(&surface).to_hex().to_string(),
            "package": "00".repeat(32),
            "publisher": "00".repeat(32),
            "dev": true,
        }),
    )
    .await
    .expect("the workspace runs a version of Chat");
    let route = super::record_route(&workspace, APP);
    assert!(
        workspaces
            .own_route_keys(&route, true)
            .await
            .expect("the workspace answers for its keys")
            .is_ok(),
        "the record's channel keys are here"
    );
    let packages = built_in(APP)
        .unwrap()
        .expect("Chat is built into this build");
    let person = hex::encode(device.cert.user_public_key);
    let viewer = member(MemberId::person(person.clone()));
    // Chat starts empty: its root is the first thing its record holds, made by the language the
    // host mounted. A kind the package's surface names but the store does not declare is refused
    // here, saying which.
    let root = with_tl_record(
        workspaces,
        &workspace,
        &packages,
        (viewer, &person),
        |opened| {
            opened
                .host()
                .root_of(APP)
                .map(|root| root.id_string())
                .map_err(|error| error.to_string())
        },
    )
    .await
    .expect("Chat's root is made: the kinds its surface names are declared by the mount");
    assert!(
        root.starts_with("root-"),
        "the record names a root resource: {root}"
    );
    device.shutdown().await;
}
