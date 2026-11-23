//! A server applies an installed app's action from the model its package
//! carries, and another holder replays the invocation with the same module.

use std::path::{Path, PathBuf};

use super::fixtures::{governor, test_host};
use super::packaged_model::PackagedAction;
use tilo_app_package::{Manifest, SigningIdentity, pack, tapp};
use tilo_model_engine::Model;
use tilo_resource::Resource as _;

/// The app the test installs, which the server does not have compiled in.
pub(super) const CLOCKS: &str = "so.tilo.clocks";

/// The resource kind the app keeps its collection under, which its module
/// routes requests to.
pub(super) const COLLECTION: &str = "clocks.collection";

/// The invocation's origin clock, milliseconds since the Unix epoch.
pub(super) const NOW_MS: i64 = 1_760_000_000_000;

/// The Clocks app's signed surface, which its package ships and its changes
/// are decided against.
const CLOCKS_SURFACE: &[u8] = include_bytes!("../../../../../apps/clocks/surface.toml");

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
pub(super) fn model_module(app: &str) -> Vec<u8> {
    let module = models_dir().join(app).join(tilo_app_package::MODEL_MODULE);
    let asked = format!(
        "reading {}; build it with `t outputs app-models`",
        module.display()
    );
    std::fs::read(&module).expect(&asked)
}

/// The app-models output: each app's module, and the model runtime beside
/// them that the engine links them after.
pub(super) fn models_dir() -> PathBuf {
    std::env::var_os("TILO_APP_MODELS").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../..")
                .join("target/app-models")
        },
        PathBuf::from,
    )
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
/// check names one (`TILO_APP_PACKAGE`, from `tools/ci/pack-clocks-tapp.sh`, so
/// what applies the action is a package an image would install), else the
/// fixture below, which stages the same payload a test can make itself.
#[expect(
    clippy::expect_used,
    reason = "a package the check said it built and is not there is a test failure"
)]
fn installed_package() -> Vec<u8> {
    std::env::var_os("TILO_APP_PACKAGE").map_or_else(
        || package(CLOCKS, Some(&model_module("clocks"))),
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
fn package(app: &str, model: Option<&[u8]>) -> Vec<u8> {
    package_on(app, model, None)
}

/// [`package`], naming `runtime` (`tilo-runtime/<version>`) as the runtime
/// bundle it was built against when given: the bundle that carries the model
/// runtime its module links against.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that cannot be written or packed is a test failure"
)]
pub(super) fn package_on(app: &str, model: Option<&[u8]>, runtime: Option<&str>) -> Vec<u8> {
    let scratch = tempfile::tempdir().unwrap();
    let payload = scratch.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/clocks"), b"the app's window").unwrap();
    std::fs::write(payload.join("surface.toml"), CLOCKS_SURFACE).unwrap();
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
{runtime}

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
bin = "bin/clocks"
type = "gui"
"#,
        hex::encode(identity.public_key_bytes()),
        runtime = runtime
            .map(|runtime| format!("runtime = \"{runtime}\""))
            .unwrap_or_default(),
    ))
    .unwrap();
    let packed = pack(&payload, &manifest, &identity).unwrap();
    let source: &dyn tilo_app_package::BlobSource = &packed;
    tapp::to_bytes(&packed.meta, Some(source)).unwrap()
}

/// A world clock for `city` as the action API binds it.
pub(super) fn add_clock(city: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("city".into(), serde_json::Value::String(city.into()));
    arguments.insert("timezone".into(), serde_json::Value::String("UTC".into()));
    arguments
}

/// What a step of the test found wrong, for the test that calls it to fail with.
type Checked<T> = Result<T, String>;

/// `Ok` when `holds`, else `expected`, what should have held, for the test to fail with.
fn check(holds: bool, expected: &str) -> Checked<()> {
    if holds {
        Ok(())
    } else {
        Err(expected.to_owned())
    }
}

/// `error` as a step reports it.
fn failed(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

/// What the installed app's action wrote, read with the app's own model code built natively here,
/// and the history entry its change keeps: the action in the bytes its own type writes, the app
/// and the person. Answers the document and the change's head.
fn what_the_action_wrote(after: &[u8]) -> Checked<(automerge::AutoCommit, automerge::ChangeHash)> {
    let document = automerge::AutoCommit::load(after).map_err(failed)?;
    let clocks = clocks_model::Clocks::read(&document).map_err(failed)?;
    check(
        clocks
            .world_clocks
            .values()
            .any(|clock| clock.city.as_str() == "Ship it"),
        "the collection holds the city the action made",
    )?;
    let mut written = automerge::AutoCommit::load(after).map_err(failed)?;
    let head = *written
        .get_heads()
        .first()
        .ok_or("the document has a head")?;
    let entry = tilo_actions::entry_of(written.committed_document(), head)
        .ok_or("the change has an entry")?;
    let recorded = entry.action.as_ref().ok_or("the entry holds the action")?;
    check(
        recorded.type_id == COLLECTION,
        "the recorded action is the tracker's",
    )?;
    check(
        String::from_utf8_lossy(&recorded.payload).contains("\"add-world-clock\""),
        "the recorded action is the verb applied",
    )?;
    check(
        entry.app.as_ref().map(|app| app.id.as_str()) == Some(CLOCKS),
        "the entry names the app the package is",
    )?;
    check(entry.invoker.is_some(), "the entry names who acted")?;
    Ok((written, head))
}

/// Another holder of the resource replays the invocation `envelope` with the module read from
/// `package` itself, at the invocation's base heads, and makes the server's change byte for byte.
fn a_holder_replays_it(
    package: &[u8],
    base: &[u8],
    envelope: serde_json::Value,
    written: &mut automerge::AutoCommit,
) -> Checked<()> {
    // The packs ship the module split, so what applied the action was the app's code linked
    // after the model runtime.
    let module = packaged_model(package);
    check(
        tilo_model_engine::linked_runtime(&module)
            .map_err(failed)?
            .is_some(),
        "the package carries the app's module split from the model runtime",
    )?;
    let model = Model::compile(&module).map_err(failed)?;
    let envelope: tilo_actions::ActionEnvelope =
        serde_json::from_value(envelope).map_err(failed)?;
    let replayed = model.replay(COLLECTION, base, &envelope).map_err(failed)?;
    let mut replayed_document = automerge::AutoCommit::load(base).map_err(failed)?;
    tilo_automerge::ApplyIncoming::load_incoming(&mut replayed_document, &replayed)
        .map_err(failed)?;
    check(
        replayed_document.get_heads() == written.get_heads(),
        "the holder's replay of the invocation makes the server's change",
    )?;
    Ok(())
}

/// An argument the signed surface does not take is refused before the module builds anything,
/// and the workspace's document stays `after`.
async fn a_wrong_argument_writes_nothing(
    workspaces: &super::Workspaces,
    workspace: &str,
    cert: &tilo_user_identity::DeviceCert,
    package: &[u8],
    after: &[u8],
) -> Checked<()> {
    let mut wrong = serde_json::Map::new();
    wrong.insert("city".into(), serde_json::Value::from(5));
    let member = tilo_actions::MemberId::person(hex::encode(cert.user_public_key));
    let refused = workspaces
        .apply_packaged_action(
            workspace,
            cert,
            PackagedAction {
                app: CLOCKS,
                kind: COLLECTION,
                document: "",
                package,
                action: "add-world-clock",
                arguments: wrong,
                member: member.clone(),
                acting: tilo_actions::Acting::member(member),
                now_ms: NOW_MS.saturating_add(1),
            },
        )
        .await;
    check(refused.is_err(), "the device refuses a wrong argument")?;
    let document = workspaces
        .resource_document(workspace, CLOCKS, "")
        .await
        .map_err(failed)?
        .ok_or("the document still exists")?;
    check(document == after, "a refused action writes nothing")?;
    Ok(())
}

/// Taking the step at `head` back is history's own revert, on the device: nothing of it reaches
/// the module.
fn the_revert_takes_it_back(
    written: &mut automerge::AutoCommit,
    head: &automerge::ChangeHash,
) -> Checked<()> {
    let authors = ui::history_source::authors::AuthorDirectory::new();
    let outcome =
        ui::history_source::revert::revert_step(written, head, None, &authors).map_err(failed)?;
    check(outcome.wrote, "the revert writes the step's inverse")?;
    written.commit();
    check(
        !clocks_model::Clocks::read(written)
            .map_err(failed)?
            .world_clocks
            .values()
            .any(|clock| clock.city.as_str() == "Ship it"),
        "the reverted step's city is gone",
    )?;
    Ok(())
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
            .prepare_packaged_application(&created.workspace, &cert, CLOCKS, COLLECTION, "", &package)
            .await
            .unwrap(),
        "the server makes the app's resource from its package"
    );
    let base = workspaces
        .resource_document(&created.workspace, CLOCKS, "")
        .await
        .unwrap()
        .expect("the workspace holds the resource the module made");
    let envelope = workspaces
        .apply_packaged_action(
            &created.workspace,
            &cert,
            PackagedAction {
                app: CLOCKS,
                kind: COLLECTION,
                document: "",
                package: &package,
                action: "add-world-clock",
                arguments: add_clock("Ship it"),
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
        .resource_document(&created.workspace, CLOCKS, "")
        .await
        .unwrap()
        .expect("the document still exists");
    let (mut written, head) = what_the_action_wrote(&after).unwrap();
    a_holder_replays_it(&package, &base, envelope, &mut written).unwrap();
    a_wrong_argument_writes_nothing(workspaces, &created.workspace, &cert, &package, &after)
        .await
        .unwrap();
    the_revert_takes_it_back(&mut written, &head).unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn a_package_without_a_model_applies_nothing() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("No model").await.unwrap();
    let package = package(CLOCKS, None);
    let refused = workspaces
        .prepare_packaged_application(&created.workspace, &cert, CLOCKS, COLLECTION, "", &package)
        .await;
    assert!(
        refused.is_err(),
        "a package with no model module installs no resource"
    );
    server.shutdown().await;
}
