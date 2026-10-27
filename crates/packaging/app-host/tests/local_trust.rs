use app_host::app_host_capnp::app_host as rpc;
use app_host::local_package::LocalReview;
use capnp_rpc::rpc_twoparty_capnp;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tilo_app_package::trust::{RevocationList, RevokedKey, TrustFile};

#[path = "../src/local_package/fixture.rs"]
mod fixture;

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn host() -> PathBuf {
    tilo_test_env::package_binary!("tilo-app-host")
}

fn installer() -> PathBuf {
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tilo-install");
    assert!(
        path.is_file(),
        "Build the actual tilo-install binary before this integration test"
    );
    path
}

fn isolated_command(binary: &Path, root: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"));
    command
}

async fn connect(socket: &Path) -> rpc::Client {
    use tokio::io::AsyncWriteExt;
    let deadline = tilo_test_env::hang_deadline();
    let mut stream = loop {
        if let Ok(stream) = tokio::net::UnixStream::connect(socket).await {
            break stream;
        }
        assert!(
            Instant::now() < deadline,
            "Private real host never became ready"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let handoff = br#"{"protocol":1,"app_id":"so.tilo.store"}"#;
    stream.write_all(b"QAH1").await.unwrap();
    stream
        .write_all(&u32::try_from(handoff.len()).unwrap().to_be_bytes())
        .await
        .unwrap();
    stream.write_all(handoff).await.unwrap();
    runtime_portal::CapnpClient::from_connected_io(stream, rpc_twoparty_capnp::Side::Client)
        .bootstrap()
}

fn review(
    client: &rpc::Client,
    bytes: &[u8],
) -> capnp::capability::Request<rpc::review_local_params::Owned, rpc::review_local_results::Owned> {
    let mut request = client.review_local_request();
    request.get().set_tapp_bytes(bytes);
    request
}

fn decoded(
    response: &capnp::capability::Response<rpc::review_local_results::Owned>,
) -> LocalReview {
    let result = response.get().unwrap();
    let json = result.get_review_json().unwrap().to_str().unwrap();
    serde_json::from_str(json).unwrap()
}

fn install(
    client: &rpc::Client,
    bytes: &[u8],
    review: &LocalReview,
) -> capnp::capability::Request<rpc::install_local_params::Owned, rpc::install_local_results::Owned>
{
    let mut request = client.install_local_request();
    let mut params = request.get();
    params.set_tapp_bytes(bytes);
    params.set_reviewed_root(&review.root);
    params.set_reviewed_archive_hash(&review.archive_hash);
    request
}

#[test]
fn local_package_real_host_installs_full_cached_thin_and_refuses_stale_review_and_revocation() {
    let root = tempfile::tempdir().unwrap();
    let (full, thin, trust, publisher) = fixture::fixture();
    let config = root.path().join("config/tilo");
    std::fs::create_dir_all(&config).unwrap();
    trust.save(&config.join("trusted-attestors.toml")).unwrap();
    let socket = root.path().join("host.sock");
    let mut command = isolated_command(&host(), root.path());
    command
        .arg("--socket")
        .arg(&socket)
        .arg("--max-store-bytes")
        .arg("0")
        .env("TILO_INSTALL_BIN", installer())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _host = Host(command.spawn().unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let client = connect(&socket).await;
        let checked = decoded(&review(&client, &full).send().promise.await.unwrap());
        assert_eq!(checked.manifest.package.id, "so.tilo.unlisted-local");
        assert!(checked.installed_version.is_none());
        // Both are valid trusted encodings of the same signed root. Review
        // of one cannot authorize replacing handed bytes with the other.
        assert!(
            install(&client, &thin, &checked)
                .send()
                .promise
                .await
                .err()
                .expect("The canonical host must refuse this install")
                .to_string()
                .contains("changed since review")
        );
        let id = install(&client, &full, &checked)
            .send()
            .promise
            .await
            .unwrap();
        assert_eq!(
            hex::encode(id.get().unwrap().get_id().unwrap()),
            checked.root
        );
        let thin_review = decoded(&review(&client, &thin).send().promise.await.unwrap());
        assert_eq!(thin_review.installed_version.as_deref(), Some("1.0.0"));
        let mut pin = isolated_command(&installer(), root.path());
        assert!(
            pin.args(["pin", "so.tilo.unlisted-local"])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(
            install(&client, &thin, &thin_review)
                .send()
                .promise
                .await
                .is_ok()
        );
        assert!(decoded(&review(&client, &thin).send().promise.await.unwrap()).pinned);
        let (replacement, _, replacement_trust, _) = fixture::fixture();
        replacement_trust
            .save(&config.join("trusted-attestors.toml"))
            .unwrap();
        let replacement_review =
            decoded(&review(&client, &replacement).send().promise.await.unwrap());
        let refusal = install(&client, &replacement, &replacement_review)
            .send()
            .promise
            .await
            .err()
            .expect("The canonical host must refuse this install")
            .to_string();
        assert!(refusal.contains("changed publisher"), "{refusal}");
        trust.save(&config.join("trusted-attestors.toml")).unwrap();
        assert_eq!(
            decoded(&review(&client, &full).send().promise.await.unwrap()).root,
            checked.root
        );
        RevocationList {
            revoked: vec![RevokedKey {
                pubkey_hex: publisher,
                reason: "fixture revocation after review".into(),
            }],
        }
        .save(&config.join("revoked-keys.toml"))
        .unwrap();
        assert!(review(&client, &full).send().promise.await.is_err());
        assert!(
            install(&client, &full, &checked)
                .send()
                .promise
                .await
                .is_err()
        );
        assert!(root.path().join("data/tilo-apps").is_dir());
    });
}
