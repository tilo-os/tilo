//! A server applies an installed app's action from the model its package
//! carries, and another holder replays the invocation with the same module.

#![expect(
    clippy::significant_drop_tightening,
    reason = "a test binds the server it drives until it shuts it down at the end"
)]

use std::path::{Path, PathBuf};

use super::fixtures::{governor, test_host};
use super::packaged_model::PackagedAction;
use tilo_app_package::{Manifest, SigningIdentity, pack, tapp};
use tilo_model_engine::{Model, Op, Request};
use tilo_resource::Resource as _;

/// The app the test installs, which the server does not have compiled in.
const TASKS: &str = "so.tilo.tasks";

/// The invocation's origin clock, milliseconds since the Unix epoch.
const NOW_MS: i64 = 1_760_000_000_000;

/// The Tasks model module, built for wasm32 by `tilo-app model` beside the app
/// (`apps/tasks-model-wasm`). A tree keeps no built module, so the check that
/// exercises this test needs `app-models`, whose outputs land under the target
/// directory; `t outputs app-models` fetches them for a test taken here.
#[expect(
    clippy::expect_used,
    reason = "a module the app-models check did not build is a test failure"
)]
fn tasks_model() -> Vec<u8> {
    let directory = std::env::var_os("TILO_APP_MODELS").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../..")
                .join("target/app-models")
        },
        PathBuf::from,
    );
    let module = directory.join("tasks").join(tilo_app_package::MODEL_MODULE);
    let asked = format!(
        "reading {}; build it with `t outputs app-models`",
        module.display()
    );
    std::fs::read(&module).expect(&asked)
}

/// An app's package: `model`, when it has one, is the payload's model module.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that cannot be written or packed is a test failure"
)]
fn package(app: &str, model: Option<&[u8]>) -> Vec<u8> {
    let scratch = tempfile::tempdir().unwrap();
    let payload = scratch.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/tasks"), b"the app's window").unwrap();
    if let Some(model) = model {
        std::fs::write(payload.join(tilo_app_package::MODEL_MODULE), model).unwrap();
    }
    let identity = SigningIdentity::generate();
    let manifest = Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{app}"
version = "1.0.0"
kind = "app"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/tasks"
type = "gui"
"#,
        hex::encode(identity.public_key_bytes())
    ))
    .unwrap();
    let packed = pack(&payload, &manifest, &identity).unwrap();
    let source: &dyn tilo_app_package::BlobSource = &packed;
    tapp::to_bytes(&packed.meta, Some(source)).unwrap()
}

/// `name` as the action API binds it.
fn create_area(name: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("name".into(), serde_json::Value::String(name.into()));
    arguments
}

#[tokio::test]
#[ignore = "reads the model module the app-models check built (t test test-packaged-model)"]
async fn a_server_applies_an_installed_apps_action_from_its_package() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Packaged model").await.unwrap();
    let model = tasks_model();
    let package = package(TASKS, Some(&model));
    assert!(
        workspaces
            .prepare_packaged_application(&created.workspace, &cert, TASKS, "", &package)
            .await
            .unwrap(),
        "the server makes the app's resource from its package"
    );
    let base = workspaces
        .resource_document(&created.workspace, TASKS, "")
        .await
        .unwrap()
        .expect("the workspace holds the resource the module made");
    let envelope = workspaces
        .apply_packaged_action(
            &created.workspace,
            &cert,
            PackagedAction {
                app: TASKS,
                document: "",
                package: &package,
                action: "create-area",
                arguments: create_area("Ship it"),
                now_ms: NOW_MS,
            },
        )
        .await
        .unwrap();
    assert!(
        envelope.get("id").is_some() && envelope.get("base_heads").is_some(),
        "the model makes the invocation a peer replays"
    );
    let after = workspaces
        .resource_document(&created.workspace, TASKS, "")
        .await
        .unwrap()
        .expect("the document still exists");
    // Read with the app's own model code, built natively here: the module
    // wrote what the app means.
    let document = automerge::AutoCommit::load(&after).unwrap();
    let tracker = tasks_model::Tracker::read(&document).unwrap();
    assert!(
        tracker
            .areas
            .iter()
            .any(|(_, area)| area.name.as_str() == "Ship it"),
        "the tracker holds the project the action made"
    );
    // Another holder of the resource replays the invocation with the same
    // module: the change it writes is the change the server published.
    let model = Model::compile(&tasks_model()).unwrap();
    let replayed = model
        .ask(&Request {
            op: Op::Apply,
            id: "",
            home: &created.workspace,
            bytes: &base,
            member: &hex::encode(cert.user_public_key),
            action: "create-area",
            arguments: create_area("Ship it"),
            now: NOW_MS,
        })
        .unwrap();
    let mut replayed_document = automerge::AutoCommit::load(&base).unwrap();
    tilo_automerge::ApplyIncoming::load_incoming(
        &mut replayed_document,
        &replayed
            .changes
            .expect("the module wrote the action's change"),
    )
    .unwrap();
    assert_eq!(
        replayed_document.get_heads(),
        automerge::AutoCommit::load(&after).unwrap().get_heads(),
        "the same module replays the action to the same document"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_package_without_a_model_applies_nothing() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("No model").await.unwrap();
    let package = package(TASKS, None);
    let refused = workspaces
        .prepare_packaged_application(&created.workspace, &cert, TASKS, "", &package)
        .await;
    assert!(
        refused.is_err(),
        "a package with no model module installs no resource"
    );
    server.shutdown().await;
}
