//! Both providers over a real D-Bus connection pair, called exactly as GNOME
//! Shell and KRunner call them.

use std::collections::HashMap;
use std::sync::Arc;

use zbus::zvariant::OwnedValue;

use super::{GNOME_PATH, KRUNNER_PATH, serve};
use crate::tests::{Host, owner, row, search};

const GNOME: &str = "org.gnome.Shell.SearchProvider2";
const KRUNNER: &str = "org.kde.krunner1";

/// A client connected to the providers, peer to peer, over a socket pair.
async fn client(host: &Host) -> (zbus::Connection, zbus::Connection) {
    let search = Arc::new(search(host));
    search.refresh().await.unwrap();
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let server = serve(
        zbus::connection::Builder::unix_stream(server)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p(),
        &search,
    )
    .unwrap()
    .build();
    let client = zbus::connection::Builder::unix_stream(client).p2p().build();
    tokio::try_join!(server, client).unwrap()
}

async fn call<B, R>(client: &zbus::Connection, path: &str, iface: &str, method: &str, body: &B) -> R
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
{
    client
        .call_method(None::<&str>, path, Some(iface), method, body)
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
    let (_server, client) = client(&host).await;
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

    let () = call(
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
    let (_server, client) = client(&host).await;
    let ids: Vec<String> = call(
        &client,
        GNOME_PATH,
        GNOME,
        "GetInitialResultSet",
        &(vec!["tax"],),
    )
    .await;
    host.edit(|catalogue| catalogue.rows.retain(|r| r.resource.id != "page:2"));
    let () = call(
        &client,
        GNOME_PATH,
        GNOME,
        "ActivateResult",
        &(ids[0].as_str(), Vec::<String>::new(), 0_u32),
    )
    .await;
    assert!(host.invoked().is_empty(), "revoked before the next walk");
    let () = call(
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
    let (_server, client) = client(&host).await;
    let actions: Vec<(String, String, String)> =
        call(&client, KRUNNER_PATH, KRUNNER, "Actions", &()).await;
    assert!(actions.is_empty());
    let matches: Vec<(
        String,
        String,
        String,
        i32,
        f64,
        HashMap<String, OwnedValue>,
    )> = call(&client, KRUNNER_PATH, KRUNNER, "Match", &("tax",)).await;
    assert_eq!(matches.len(), 1, "{matches:?}");
    let (id, text, _icon, _category, relevance, _properties) = &matches[0];
    assert_eq!(text, "Tax return");
    assert!(*relevance > 0.0 && *relevance <= 1.0);

    let () = call(&client, KRUNNER_PATH, KRUNNER, "Run", &(id.as_str(), "")).await;
    let invoked = host.invoked();
    assert_eq!(invoked.len(), 1);
    assert_eq!(
        invoked[0].1.target.address,
        tilo_actions::ActionAddress::new("page:2", "open")
    );
    let none: Vec<(
        String,
        String,
        String,
        i32,
        f64,
        HashMap<String, OwnedValue>,
    )> = call(&client, KRUNNER_PATH, KRUNNER, "Match", &("groceries",)).await;
    assert!(none.is_empty());
}
