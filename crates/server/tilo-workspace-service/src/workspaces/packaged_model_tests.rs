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

/// The resource kind of the app's tracker, which its module routes requests
/// to.
const TRACKER: &str = "tasks.tracker";

/// The invocation's origin clock, milliseconds since the Unix epoch.
const NOW_MS: i64 = 1_760_000_000_000;

/// The Tasks app's signed surface, which its package ships and its changes are
/// decided against.
const TASKS_SURFACE: &[u8] = include_bytes!("../../../../../apps/tasks/surface.toml");

/// The app's model module the `app-models` check built (tools/ci/app-models.sh)
/// from the guest crate beside the app (`apps/<app>-model-wasm`): the app's own
/// code, which links against the model runtime beside it under
/// `TILO_APP_MODELS`, where the engine finds it as a device finds it. A tree
/// keeps no built module, so the fixture path that needs one reads that output;
/// the check itself hands this test a package from the pack path, and the test
/// reads that package's own module.
#[expect(
    clippy::expect_used,
    reason = "a module the app-models check did not build is a test failure"
)]
fn model_module(app: &str) -> Vec<u8> {
    let directory = std::env::var_os("TILO_APP_MODELS").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../..")
                .join("target/app-models")
        },
        PathBuf::from,
    );
    let module = directory.join(app).join(tilo_app_package::MODEL_MODULE);
    let asked = format!(
        "reading {}; build it with `t outputs app-models`",
        module.display()
    );
    std::fs::read(&module).expect(&asked)
}

/// The model module a package carries: the bytes every device applies the
/// app's actions with, as the package's signed root names them.
#[expect(
    clippy::expect_used,
    reason = "a package the pack path built without its model is a test failure"
)]
fn packaged_model(package: &[u8]) -> Vec<u8> {
    let mut blobs = std::collections::BTreeMap::new();
    let package = tapp::read(package, &mut blobs).expect("the package reads");
    let hash = package
        .meta
        .collection
        .entries
        .get(tilo_app_package::MODEL_MODULE)
        .expect("the package carries its model module");
    blobs
        .get(hash)
        .cloned()
        .expect("the package carries the module's bytes")
}

/// The package the server installs: the image's own pack path built it when the
/// check names one (`TILO_APP_PACKAGE`, from `tools/ci/pack-tasks-tapp.sh`, so
/// what applies the action is a package an image would install), else the
/// fixture below, which stages the same payload a test can make itself.
#[expect(
    clippy::expect_used,
    reason = "a package the check said it built and is not there is a test failure"
)]
fn installed_package() -> Vec<u8> {
    std::env::var_os("TILO_APP_PACKAGE").map_or_else(
        || package(TASKS, Some(&model_module("tasks"))),
        |path| {
            let path = PathBuf::from(path);
            let asked = format!(
                "reading {}; build it with `t test test-packaged-model`",
                path.display()
            );
            std::fs::read(&path).expect(&asked)
        },
    )
}

/// An app's package: `model`, when it has one, is the payload's model module,
/// beside the surface the app ships.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that cannot be written or packed is a test failure"
)]
fn package(app: &str, model: Option<&[u8]>) -> Vec<u8> {
    let scratch = tempfile::tempdir().unwrap();
    let payload = scratch.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/tasks"), b"the app's window").unwrap();
    std::fs::write(payload.join("surface.toml"), TASKS_SURFACE).unwrap();
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
    let package = installed_package();
    assert!(
        workspaces
            .prepare_packaged_application(&created.workspace, &cert, TASKS, TRACKER, "", &package)
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
                kind: TRACKER,
                document: "",
                package: &package,
                action: "create-area",
                arguments: create_area("Ship it"),
                member: tilo_actions::MemberId::person(hex::encode(cert.user_public_key)),
                acting: tilo_actions::Acting::member(tilo_actions::MemberId::person(hex::encode(
                    cert.user_public_key,
                ))),
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
    // module, read from the package itself: the change it writes is the change
    // the server published. The packs ship it split, so what applied the
    // action above was the app's code linked after the model runtime.
    let module = packaged_model(&package);
    assert!(
        tilo_model_engine::linked_runtime(&module)
            .unwrap()
            .is_some(),
        "the package carries the app's module split from the model runtime"
    );
    let model = Model::compile(&module).unwrap();
    let replayed = model
        .ask(&Request {
            op: Op::Apply,
            kind: TRACKER,
            id: "",
            home: &created.workspace,
            bytes: &base,
            member: &hex::encode(cert.user_public_key),
            acting: None,
            action: "create-area",
            arguments: create_area("Ship it"),
            now: NOW_MS,
            envelope: None,
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
        .prepare_packaged_application(&created.workspace, &cert, TASKS, TRACKER, "", &package)
        .await;
    assert!(
        refused.is_err(),
        "a package with no model module installs no resource"
    );
    server.shutdown().await;
}

/// One module serves every resource its app declares, told apart by the kind
/// each request names: Tube's library and its watch history come from the same
/// bytes, and each kind's own verbs answer it.
#[test]
#[ignore = "reads the model module the app-models check built (t test test-packaged-model)"]
fn one_module_serves_every_resource_of_its_app() {
    const LIBRARY: &str = "tube.library";
    const HISTORY: &str = "tube.watch-history";
    let model = Model::compile(&model_module("tube")).unwrap();
    let member = hex::encode([7_u8; 32]);
    let ask = |op: Op, kind: &str, bytes: &[u8], action: &str, arguments| {
        model
            .ask(&Request {
                op,
                kind,
                id: "",
                home: "a workspace",
                bytes,
                member: &member,
                acting: None,
                action,
                arguments,
                now: NOW_MS,
                envelope: None,
            })
            .unwrap()
    };
    let genesis = |kind: &str| {
        ask(Op::Initialize, kind, &[], "", serde_json::Map::new())
            .document
            .expect("the module made the resource's genesis")
    };
    let library = automerge::AutoCommit::load(&genesis(LIBRARY)).unwrap();
    tube_model::Library::read(&library).expect("the library's own code reads its genesis");
    let history_document = genesis(HISTORY);
    let mut history = automerge::AutoCommit::load(&history_document).unwrap();
    tube_model::WatchHistory::read(&history)
        .expect("the watch history's own code reads its genesis");
    let mut arguments = serde_json::Map::new();
    arguments.insert("video".into(), serde_json::Value::from("a video"));
    arguments.insert("position_ms".into(), serde_json::Value::from(5_u64));
    arguments.insert("duration_ms".into(), serde_json::Value::from(600_000_u64));
    let played = ask(
        Op::Apply,
        HISTORY,
        &history_document,
        tube_model::WatchHistory::PROGRESS.id(),
        arguments,
    );
    tilo_automerge::ApplyIncoming::load_incoming(
        &mut history,
        &played.changes.expect("the module wrote the change"),
    )
    .unwrap();
    assert!(
        tube_model::WatchHistory::read(&history)
            .unwrap()
            .positions
            .contains_key("a video"),
        "the request naming the watch history ran that resource's verb"
    );
    let refused = model.ask(&Request {
        op: Op::Initialize,
        kind: "tube.nothing",
        id: "",
        home: "a workspace",
        bytes: &[],
        member: &member,
        acting: None,
        action: "",
        arguments: serde_json::Map::new(),
        now: NOW_MS,
        envelope: None,
    });
    assert!(
        refused.is_err(),
        "the module refuses a request for a kind it does not serve"
    );
}
