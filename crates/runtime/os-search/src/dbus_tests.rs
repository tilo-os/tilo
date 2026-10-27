use std::collections::HashMap;
use std::sync::Arc;

use zbus::zvariant::OwnedValue;

use super::{BUS_NAME, GNOME_PATH, KRUNNER_PATH, serve_on};
use crate::tests::{Host, owner, row, search};

const GNOME: &str = "org.gnome.Shell.SearchProvider2";
const KRUNNER: &str = "org.kde.krunner1";

/// One `org.kde.krunner1` match: id, text, icon, category, relevance, properties.
type Match = (
    String,
    String,
    String,
    i32,
    f64,
    HashMap<String, OwnedValue>,
);

/// A session bus of its own, as `dbus-run-session` starts one, stopped on drop.
struct SessionBus {
    daemon: std::process::Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl SessionBus {
    fn start() -> Self {
        use std::io::BufRead;
        let directory = tempfile::tempdir().unwrap();
        let mut daemon = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!(
                "--address=unix:path={}",
                directory.path().join("bus").display()
            ))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the session bus needs dbus-daemon, which the builder image installs");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty(), "dbus-daemon printed no address");
        Self {
            daemon,
            address: address.trim().to_owned(),
            _directory: directory,
        }
    }
}

impl Drop for SessionBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// The providers serving on a fresh session bus, and a client on the same bus.
async fn client(host: &Host) -> (SessionBus, zbus::Connection, zbus::Connection) {
    let search = Arc::new(search(host));
    search.refresh().await.unwrap();
    let bus = SessionBus::start();
    let server = serve_on(
        zbus::connection::Builder::address(bus.address.as_str()).unwrap(),
        &search,
    )
    .await
    .unwrap();
    let client = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    (bus, server, client)
}

async fn call<B, R>(client: &zbus::Connection, path: &str, iface: &str, method: &str, body: &B) -> R
where
    B: serde::Serialize + zbus::zvariant::DynamicType + Sync,
    R: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
{
    client
        .call_method(Some(BUS_NAME), path, Some(iface), method, body)
        .await
        .unwrap()
        .body()
        .deserialize()
        .unwrap()
}

fn catalogue() -> Host {
    let host = Host::default();
    host.edit(|catalogue| {
        catalogue.rows = vec![
            row("7", "page:1", "Travel plans", "open"),
            row("7", "page:2", "Tax return", "open"),
        ];
    });
    host
}

#[tokio::test]
async fn gnome_shell_finds_describes_and_opens_through_the_action_host() {
    let host = catalogue();
    let (_bus, _server, client) = client(&host).await;
    let ids: Vec<String> = call(
        &client,
        GNOME_PATH,
        GNOME,
        "GetInitialResultSet",
        &(vec!["trav"],),
    )
    .await;
    assert_eq!(ids.len(), 1, "{ids:?}");
    let narrowed: Vec<String> = call(
        &client,
        GNOME_PATH,
        GNOME,
        "GetSubsearchResultSet",
        &(ids.clone(), vec!["travel", "plans"]),
    )
    .await;
    assert_eq!(narrowed, ids);

    let metas: Vec<HashMap<String, OwnedValue>> = call(
        &client,
        GNOME_PATH,
        GNOME,
        "GetResultMetas",
        &(vec![ids[0].clone(), "ffffffffffffffff".to_owned()],),
    )
    .await;
    assert_eq!(metas.len(), 1, "an unknown id has no meta");
    assert_eq!(String::try_from(metas[0]["id"].clone()).unwrap(), ids[0]);
    assert_eq!(
        String::try_from(metas[0]["name"].clone()).unwrap(),
        "Travel plans"
    );

    call::<_, ()>(
        &client,
        GNOME_PATH,
        GNOME,
        "ActivateResult",
        &(ids[0].as_str(), vec!["trav"], 0_u32),
    )
    .await;
    let invoked = host.invoked();
    assert_eq!(invoked.len(), 1);
    assert_eq!(invoked[0].0, owner());
    assert_eq!(
        invoked[0].1.target.address,
        tilo_actions::ActionAddress::new("page:1", "open")
    );
}

#[tokio::test]
async fn gnome_shell_opens_nothing_the_owner_may_no_longer_open() {
    let host = catalogue();
    let (_bus, _server, client) = client(&host).await;
    let ids: Vec<String> = call(
        &client,
        GNOME_PATH,
        GNOME,
        "GetInitialResultSet",
        &(vec!["tax"],),
    )
    .await;
    host.edit(|catalogue| catalogue.rows.retain(|r| r.resource.id != "page:2"));
    call::<_, ()>(
        &client,
        GNOME_PATH,
        GNOME,
        "ActivateResult",
        &(ids[0].as_str(), Vec::<String>::new(), 0_u32),
    )
    .await;
    assert!(host.invoked().is_empty(), "revoked before the next walk");
    call::<_, ()>(
        &client,
        GNOME_PATH,
        GNOME,
        "ActivateResult",
        &("not-an-id", Vec::<String>::new(), 0_u32),
    )
    .await;
    assert!(host.invoked().is_empty());
}

#[tokio::test]
async fn krunner_matches_and_runs_through_the_action_host() {
    let host = catalogue();
    let (_bus, _server, client) = client(&host).await;
    let actions: Vec<(String, String, String)> =
        call(&client, KRUNNER_PATH, KRUNNER, "Actions", &()).await;
    assert!(actions.is_empty());
    let matches: Vec<Match> = call(&client, KRUNNER_PATH, KRUNNER, "Match", &("tax",)).await;
    assert_eq!(matches.len(), 1, "{matches:?}");
    let (id, text, _icon, _category, relevance, _properties) = &matches[0];
    assert_eq!(text, "Tax return");
    assert!(*relevance > 0.0 && *relevance <= 1.0);

    call::<_, ()>(&client, KRUNNER_PATH, KRUNNER, "Run", &(id.as_str(), "")).await;
    let invoked = host.invoked();
    assert_eq!(invoked.len(), 1);
    assert_eq!(
        invoked[0].1.target.address,
        tilo_actions::ActionAddress::new("page:2", "open")
    );
    let none: Vec<Match> = call(&client, KRUNNER_PATH, KRUNNER, "Match", &("groceries",)).await;
    assert!(none.is_empty());
}
