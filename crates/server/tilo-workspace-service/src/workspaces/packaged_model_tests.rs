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
use tilo_model_engine::{Invocation, Model};
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

/// The app's model module, built for wasm32 by `tilo-app model` beside the app
/// (`apps/<app>-model-wasm`). A tree keeps no built module, so the check that
/// exercises this test needs `app-models`, whose outputs land under the target
/// directory; `t outputs app-models` fetches them for a test taken here.
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
    let model = model_module("tasks");
    let package = package(TASKS, Some(&model));
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
    // Another holder of the resource replays the invocation the server
    // recorded with the same module, against its own copy of the document at
    // the invocation's base heads: the verb reads and writes that holder's
    // document through the same document calls, and the change it writes is the
    // change the server published.
    // History keeps what was done, as the device wrote it: the action, in
    // the bytes its own type writes, the app and the person.
    let mut written = automerge::AutoCommit::load(&after).unwrap();
    let head = written.get_heads()[0];
    let entry = tilo_actions::entry_of(written.document(), head).expect("the change has an entry");
    let recorded = entry.action.as_ref().expect("the entry holds the action");
    assert_eq!(recorded.type_id, TRACKER);
    assert!(
        String::from_utf8_lossy(&recorded.payload).contains("\"create-area\""),
        "the recorded action is the verb applied"
    );
    assert_eq!(
        entry.app.as_ref().map(|app| app.id.as_str()),
        Some(TASKS),
        "the entry names the app the package is"
    );
    assert!(entry.invoker.is_some(), "the entry names who acted");

    // Another holder replays the invocation through the same module, at its
    // base heads: it makes the server's change, byte for byte.
    let model = Model::compile(&model_module("tasks")).unwrap();
    let envelope: tilo_actions::ActionEnvelope = serde_json::from_value(envelope).unwrap();
    let replayed = model.replay(TRACKER, &base, &envelope).unwrap();
    let mut replayed_document = automerge::AutoCommit::load(&base).unwrap();
    tilo_automerge::ApplyIncoming::load_incoming(&mut replayed_document, &replayed).unwrap();
    assert_eq!(
        replayed_document.get_heads(),
        written.get_heads(),
        "the holder's replay of the invocation makes the server's change"
    );

    // Taking the step back is history's own revert, on the device: nothing of
    // it reaches the module.
    let authors = ui::history_source::authors::AuthorDirectory::new();
    let outcome =
        ui::history_source::revert::revert_step(&mut written, &head, None, &authors).unwrap();
    assert!(outcome.wrote, "the revert writes the step's inverse");
    written.commit();
    assert!(
        !tasks_model::Tracker::read(&written)
            .unwrap()
            .areas
            .iter()
            .any(|(_, area)| area.name.as_str() == "Ship it"),
        "the reverted step's area is gone"
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
    let genesis = |kind: &str| {
        model
            .initialize(kind, "", "a workspace")
            .expect("the module made the resource's genesis")
    };
    let library = automerge::AutoCommit::load(&genesis(LIBRARY)).unwrap();
    tube_model::Library::read(&library).expect("the library's own code reads its genesis");
    let history_document = genesis(HISTORY);
    let mut history = automerge::AutoCommit::load(&history_document).unwrap();
    tube_model::WatchHistory::read(&history)
        .expect("the watch history's own code reads its genesis");
    let surface = collab_types::surface::SurfaceFile::parse(include_bytes!(
        "../../../../../apps/tube/surface.toml"
    ))
    .unwrap();
    let verb = surface
        .resources
        .iter()
        .filter(|resource| resource.kind == HISTORY)
        .flat_map(|resource| resource.verbs.iter())
        .find(|verb| verb.id == tube_model::WatchHistory::PROGRESS.id())
        .expect("the surface declares the verb");
    let mut arguments = serde_json::Map::new();
    arguments.insert("video".into(), serde_json::Value::from("a video"));
    arguments.insert("position_ms".into(), serde_json::Value::from(5_u64));
    arguments.insert("duration_ms".into(), serde_json::Value::from(600_000_u64));
    let member = tilo_actions::MemberId::person(hex::encode([7_u8; 32]));
    let acting = tilo_actions::Acting::member(member.clone());
    let invocation = |arguments| Invocation {
        app: "so.tilo.tube",
        kind: HISTORY,
        verb,
        id: "",
        document: &history_document,
        member: &member,
        acting: &acting,
        arguments,
        now: NOW_MS,
    };
    let played = model.apply(invocation(arguments.clone())).unwrap();
    tilo_automerge::ApplyIncoming::load_incoming(&mut history, &played.changes).unwrap();
    assert!(
        tube_model::WatchHistory::read(&history)
            .unwrap()
            .positions
            .contains_key("a video"),
        "the request naming the watch history ran that resource's verb"
    );
    let mut wrong = arguments;
    wrong.insert("position_ms".into(), serde_json::Value::from("five"));
    assert!(
        model.apply(invocation(wrong)).is_err(),
        "the device refuses an argument of the wrong type"
    );
    assert!(
        model.initialize("tube.nothing", "", "a workspace").is_err(),
        "the module refuses a request for a kind it does not serve"
    );
}
