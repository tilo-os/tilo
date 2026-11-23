//! A developer's key installs a version of an app through the action API:
//! `workspace/apps#upload-app-file`,
//! `workspace/apps#install-app` over
//! `POST /v1/workspaces/{workspace}/resources/…/actions/…`, the same path
//! `tilo-app push` takes. The workspace decides like any of its governance, and
//! every member reads the version it uses from it.

use super::api::ISSUER;
use super::api_on_server::{api_on_server, approved_key};
use super::*;
use serde_json::json;

/// The bytes of a package file one `upload-app-file` piece carries.
const PIECE: usize = 256 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_installs_an_app_version_and_asks_for_a_bot_through_the_action_api() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    // Her key reaches the API: a device of hers, as `tilo login` keeps one
    // here, is what the client below acts as.
    approved_key(&on_server, &ada, &page, "Ada's builds").await;
    let laptop = enrol(&ada, "Ada's laptop", DeviceCapabilities::member());
    let keys = tempfile::tempdir().unwrap();
    let key_file = keys.path().join("laptop.json");
    std::fs::write(
        &key_file,
        serde_json::to_vec(
            &tilo_api::direct::KeyFile::new(
                &on_server.server,
                Some(&on_server.web_entry),
                "Ada's laptop",
                &laptop.key,
                &laptop.cert,
            )
            .reaching_at(on_server.hosted.web.host_info().clone()),
        )
        .unwrap(),
    )
    .unwrap();
    let client = tilo_api::client::Client::open(tilo_api::client::Reach {
        api: None,
        key_file: Some(&key_file),
        json: false,
    })
    .unwrap();
    let made = client
        .post(
            "/v1/resources/account/workspaces/actions/create",
            &json!({"name": "Ada's builds"}),
        )
        .await
        .unwrap();
    let workspace = made["result"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Hand the workspace her package in the pieces the server takes, then
    // make its version the one the workspace uses.
    let package = b"a developer's own build of the notes app".repeat(20000);
    let file = blake3::hash(&package).to_hex().to_string();
    let total = package.len() as u64;
    let mut offset = 0_u64;
    for piece in package.chunks(PIECE) {
        let answer = client
            .act(
                &workspace,
                "workspace/apps",
                "upload-app-file",
                &json!({
                    "file": file,
                    "offset": offset,
                    "total": total,
                    "data": hex::encode(piece),
                }),
                "hand the workspace an app version's package",
            )
            .await
            .unwrap();
        assert_eq!(answer["status"], "ok", "{answer}");
        offset += piece.len() as u64;
    }
    let installed = client
        .act(
            &workspace,
            "workspace/apps",
            "install-app",
            &json!({
                "app": "so.tilo.notes",
                "version": "2.0.0-dev.1",
                "surface": hex::encode([2_u8; 32]),
                "package": hex::encode([3_u8; 32]),
                "publisher": hex::encode(laptop.cert.user_public_key),
                "dev": true,
                "file": file,
            }),
            "install a version of the app",
        )
        .await
        .unwrap();
    assert_eq!(installed["status"], "ok", "{installed}");
    assert_eq!(
        installed["result"]["running"]["release"]["version"],
        "2.0.0-dev.1"
    );
    let workspaces = on_server.hosted.server.workspaces().unwrap();
    let running = workspaces.app_versions(&workspace).await.unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].release.version, "2.0.0-dev.1");
    assert_eq!(running[0].release.file.as_deref(), Some(file.as_str()));

    // Taking the version back is the same path, and leaves the workspace with
    // no version of the app.
    let removed = client
        .act(
            &workspace,
            "workspace/apps",
            "uninstall-app",
            &json!({"app": "so.tilo.notes"}),
            "let everyone use their own version",
        )
        .await
        .unwrap();
    assert_eq!(removed["status"], "ok", "{removed}");
    assert!(
        workspaces
            .app_versions(&workspace)
            .await
            .unwrap()
            .is_empty()
    );
}
