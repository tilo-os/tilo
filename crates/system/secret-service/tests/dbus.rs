//! The Secret Service API over a real D-Bus message flow, driven the way
//! libsecret drives it: open a `dh-ietf1024-sha256-aes128-cbc-pkcs7` (or
//! `plain`) session, then create, search, read and delete through the
//! collection `default` alias.
//!
//! Each client is one peer-to-peer connection to its own server end, with a
//! fixed caller identity in place of the bus daemon's process credentials;
//! all of them share one store, as separate applications do.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tilo_seal::SymmetricKey;
use tilo_seal::secret_service::{DH_ALGORITHM, Handshake, TransportKey};
use tilo_secret_service::app::{AppId, UnknownCaller};
use tilo_secret_service::service::{
    BUS_NAME, CallerResolver, Core, MANAGEMENT_PATH, Managers, ROOT, WireSecret, publish,
};
use tilo_secret_service::store::{KeySource, Locked, Store};
use zbus::connection::Builder;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
const DEFAULT: &str = "/org/freedesktop/secrets/aliases/default";

struct Identity(AppId);

impl CallerResolver for Identity {
    fn identify<'a>(
        &'a self,
        _sender: Option<&'a str>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<AppId, UnknownCaller>> + Send + 'a>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

/// An identity that can be present or missing, like the device key.
#[derive(Clone)]
struct DeviceKey(Arc<AtomicU8>);

impl KeySource for DeviceKey {
    fn release(&self) -> Result<SymmetricKey, Locked> {
        match self.0.load(Ordering::SeqCst) {
            0 => Err(Locked::NoIdentity),
            byte => Ok(SymmetricKey::from_bytes([byte; 32])),
        }
    }
}

struct World {
    dir: tempfile::TempDir,
    store: Arc<Store>,
    device: DeviceKey,
    managers: Managers,
}

impl World {
    fn new(identity_present: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let device = DeviceKey(Arc::new(AtomicU8::new(u8::from(identity_present) * 5)));
        let store = Arc::new(Store::new(
            dir.path().join("vault"),
            Box::new(device.clone()),
        ));
        let managers = Managers {
            settings: vec![app("binary:/usr/bin/tilo-secret-service")],
            portal: vec![app("binary:/usr/bin/shell")],
        };
        Self {
            dir,
            store,
            device,
            managers,
        }
    }

    /// A client connected as `identity`.
    async fn client(&self, identity: &str) -> Connection {
        let core = Core::new(
            Arc::clone(&self.store),
            Arc::new(Identity(app(identity))),
            self.managers.clone(),
        );
        let (server_end, client_end) = tokio::net::UnixStream::pair().unwrap();
        let guid = zbus::Guid::generate();
        let server = Builder::unix_stream(server_end)
            .server(guid)
            .unwrap()
            .p2p()
            .build();
        let client = Builder::unix_stream(client_end).p2p().build();
        let (server, client) = futures_util::try_join!(server, client).unwrap();
        publish(&server, &core).await.unwrap();
        // The server end must outlive the test body.
        std::mem::forget(server);
        client
    }
}

fn app(id: &str) -> AppId {
    AppId::parse(id).unwrap()
}

/// Uncached: a test reads a property twice to watch it change, which a
/// caching client would answer from its first read.
async fn proxy<'a>(
    connection: &'a Connection,
    path: &'a str,
    interface: &'static str,
) -> Proxy<'a> {
    zbus::proxy::Builder::<Proxy<'a>>::new(connection)
        .destination(BUS_NAME)
        .unwrap()
        .path(path)
        .unwrap()
        .interface(interface)
        .unwrap()
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap()
}

/// libsecret's session: negotiate DH and keep the key.
async fn dh_session(connection: &Connection) -> (OwnedObjectPath, TransportKey) {
    let handshake = Handshake::new().unwrap();
    let public = handshake.public().to_vec();
    let service = proxy(connection, ROOT, SERVICE).await;
    let (output, path): (OwnedValue, OwnedObjectPath) = service
        .call("OpenSession", &(DH_ALGORITHM, Value::from(public)))
        .await
        .unwrap();
    let server_public = Vec::<u8>::try_from(output).unwrap();
    (path, handshake.finish(&server_public).unwrap())
}

async fn plain_session(connection: &Connection) -> OwnedObjectPath {
    let service = proxy(connection, ROOT, SERVICE).await;
    let (_, path): (OwnedValue, OwnedObjectPath) = service
        .call("OpenSession", &("plain", Value::from("")))
        .await
        .unwrap();
    path
}

fn encrypted(session: &OwnedObjectPath, key: &TransportKey, plain: &[u8]) -> WireSecret {
    let (iv, ciphertext) = key.encrypt(plain).unwrap();
    assert_ne!(
        ciphertext, plain,
        "the secret must not cross the bus in the clear"
    );
    WireSecret(
        session.clone(),
        iv.to_vec(),
        ciphertext,
        "text/plain".into(),
    )
}

fn attributes(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

async fn store_password(
    connection: &Connection,
    session: &OwnedObjectPath,
    key: &TransportKey,
    label: &str,
    attrs: &[(&str, &str)],
    secret: &[u8],
) -> OwnedObjectPath {
    let collection = proxy(connection, DEFAULT, COLLECTION).await;
    let properties: HashMap<&str, Value<'_>> = HashMap::from([
        ("org.freedesktop.Secret.Item.Label", Value::from(label)),
        (
            "org.freedesktop.Secret.Item.Attributes",
            Value::from(attributes(attrs)),
        ),
    ]);
    let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = collection
        .call(
            "CreateItem",
            &(properties, encrypted(session, key, secret), true),
        )
        .await
        .unwrap();
    assert_eq!(prompt.as_str(), "/", "storing never prompts");
    item
}

async fn lookup(
    connection: &Connection,
    attrs: &[(&str, &str)],
) -> Result<Vec<OwnedObjectPath>, zbus::Error> {
    let service = proxy(connection, ROOT, SERVICE).await;
    let (unlocked, _locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) =
        service.call("SearchItems", &(attributes(attrs),)).await?;
    Ok(unlocked)
}

async fn read_password(
    connection: &Connection,
    session: &OwnedObjectPath,
    key: &TransportKey,
    item: &OwnedObjectPath,
) -> Vec<u8> {
    let item = proxy(connection, item.as_str(), ITEM).await;
    let (secret,): (WireSecret,) = item.call("GetSecret", &(session,)).await.unwrap();
    key.decrypt(&secret.1, &secret.2).unwrap().to_vec()
}

fn error_name(error: &zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(name, _, _) => name.to_string(),
        other => format!("{other}"),
    }
}

#[tokio::test]
async fn a_libsecret_client_can_store_find_read_and_delete_over_an_encrypted_session() {
    let world = World::new(true);
    let client = world.client("binary:/usr/bin/chromium").await;
    let (session, key) = dh_session(&client).await;

    let item = store_password(
        &client,
        &session,
        &key,
        "Chrome Safe Storage",
        &[
            ("application", "chrome"),
            ("xdg:schema", "chrome_libsecret_os_crypt_password_v2"),
        ],
        b"hunter2",
    )
    .await;
    assert!(
        item.as_str()
            .starts_with("/org/freedesktop/secrets/collection/login/")
    );

    let found = lookup(&client, &[("application", "chrome")]).await.unwrap();
    assert_eq!(found, vec![item.clone()]);
    assert!(
        lookup(&client, &[("application", "firefox")])
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        read_password(&client, &session, &key, &item).await,
        b"hunter2"
    );

    // GetSecrets, the call secret_password_lookup uses.
    let service = proxy(&client, ROOT, SERVICE).await;
    let secrets: HashMap<OwnedObjectPath, WireSecret> = service
        .call("GetSecrets", &(vec![item.clone()], &session))
        .await
        .unwrap();
    assert_eq!(
        key.decrypt(&secrets[&item].1, &secrets[&item].2)
            .unwrap()
            .as_slice(),
        b"hunter2"
    );

    // The item describes itself, and the collection lists exactly the items
    // its reader may see: what clients that browse through `Items` rely on.
    let item_proxy = proxy(&client, item.as_str(), ITEM).await;
    assert_eq!(
        item_proxy.get_property::<String>("Label").await.unwrap(),
        "Chrome Safe Storage"
    );
    assert_eq!(
        item_proxy
            .get_property::<HashMap<String, String>>("Attributes")
            .await
            .unwrap()["application"],
        "chrome"
    );
    assert!(!item_proxy.get_property::<bool>("Locked").await.unwrap());
    let collection = proxy(&client, DEFAULT, COLLECTION).await;
    assert_eq!(
        collection
            .get_property::<Vec<OwnedObjectPath>>("Items")
            .await
            .unwrap(),
        vec![item.clone()]
    );

    // secret_password_store again with the same attributes replaces.
    let again = store_password(
        &client,
        &session,
        &key,
        "Chrome Safe Storage",
        &[
            ("application", "chrome"),
            ("xdg:schema", "chrome_libsecret_os_crypt_password_v2"),
        ],
        b"hunter3",
    )
    .await;
    assert_eq!(again, item);
    assert_eq!(
        read_password(&client, &session, &key, &item).await,
        b"hunter3"
    );

    // SetSecret, then Delete.
    let new = encrypted(&session, &key, b"hunter4");
    item_proxy
        .call::<_, _, ()>("SetSecret", &(new,))
        .await
        .unwrap();
    assert_eq!(
        read_password(&client, &session, &key, &item).await,
        b"hunter4"
    );
    let prompt: OwnedObjectPath = item_proxy.call("Delete", &()).await.unwrap();
    assert_eq!(prompt.as_str(), "/");
    assert!(
        lookup(&client, &[("application", "chrome")])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_plain_session_and_unsupported_algorithms_behave_as_the_specification_says() {
    let world = World::new(true);
    let client = world.client("binary:/usr/bin/chromium").await;
    let session = plain_session(&client).await;

    let collection = proxy(&client, DEFAULT, COLLECTION).await;
    let properties: HashMap<&str, Value<'_>> = HashMap::from([
        ("org.freedesktop.Secret.Item.Label", Value::from("plain")),
        (
            "org.freedesktop.Secret.Item.Attributes",
            Value::from(attributes(&[("k", "v")])),
        ),
    ]);
    let secret = WireSecret(
        session.clone(),
        Vec::new(),
        b"visible".to_vec(),
        "text/plain".into(),
    );
    let (item, _): (OwnedObjectPath, OwnedObjectPath) = collection
        .call("CreateItem", &(properties, secret, true))
        .await
        .unwrap();
    let item = proxy(&client, item.as_str(), ITEM).await;
    let (secret,): (WireSecret,) = item.call("GetSecret", &(&session,)).await.unwrap();
    assert_eq!(secret.2, b"visible");

    let service = proxy(&client, ROOT, SERVICE).await;
    let unsupported = service
        .call::<_, _, (OwnedValue, OwnedObjectPath)>("OpenSession", &("rot13", Value::from("")))
        .await
        .unwrap_err();
    assert_eq!(
        error_name(&unsupported),
        "org.freedesktop.DBus.Error.NotSupported"
    );
    // A degenerate Diffie-Hellman value is refused rather than accepted.
    let degenerate = service
        .call::<_, _, (OwnedValue, OwnedObjectPath)>(
            "OpenSession",
            &(DH_ALGORITHM, Value::from(vec![1_u8])),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error_name(&degenerate),
        "org.freedesktop.DBus.Error.InvalidArgs"
    );
    // A session is closed with Close, and then cannot be used.
    let session_proxy = proxy(&client, session.as_str(), "org.freedesktop.Secret.Session").await;
    session_proxy.call::<_, _, ()>("Close", &()).await.unwrap();
    assert!(
        item.call::<_, _, (WireSecret,)>("GetSecret", &(&session,))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn before_the_identity_exists_the_store_is_locked_and_never_prompts_then_opens_without_a_password()
 {
    let world = World::new(false);
    let client = world.client("binary:/usr/bin/chromium").await;
    let (session, key) = dh_session(&client).await;
    let service = proxy(&client, ROOT, SERVICE).await;

    // The aliases libsecret asks for still answer.
    let alias: OwnedObjectPath = service.call("ReadAlias", &("default",)).await.unwrap();
    assert_eq!(alias.as_str(), DEFAULT);
    let collection = proxy(&client, DEFAULT, COLLECTION).await;
    assert!(collection.get_property::<bool>("Locked").await.unwrap());

    // Everything else reports IsLocked.
    let error = lookup(&client, &[("a", "b")]).await.unwrap_err();
    assert_eq!(error_name(&error), "org.freedesktop.Secret.Error.IsLocked");
    let properties: HashMap<&str, Value<'_>> = HashMap::from([
        ("org.freedesktop.Secret.Item.Label", Value::from("x")),
        (
            "org.freedesktop.Secret.Item.Attributes",
            Value::from(attributes(&[])),
        ),
    ]);
    let refused = collection
        .call::<_, _, (OwnedObjectPath, OwnedObjectPath)>(
            "CreateItem",
            &(properties.clone(), encrypted(&session, &key, b"s"), true),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error_name(&refused),
        "org.freedesktop.Secret.Error.IsLocked"
    );

    // Unlock reports nothing unlocked and offers no prompt to loop on.
    let (unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = service
        .call("Unlock", &(vec![alias.clone()],))
        .await
        .unwrap();
    assert!(unlocked.is_empty());
    assert_eq!(prompt.as_str(), "/");

    // The device identity arrives (a fingerprint or automatic login is
    // enough): the same connection now works, with no unlock step.
    world.device.0.store(9, Ordering::SeqCst);
    assert!(!collection.get_property::<bool>("Locked").await.unwrap());
    let (unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = service
        .call("Unlock", &(vec![alias.clone()],))
        .await
        .unwrap();
    assert_eq!(unlocked, vec![alias]);
    assert_eq!(prompt.as_str(), "/");
    let item = store_password(&client, &session, &key, "x", &[("a", "b")], b"s").await;
    assert_eq!(lookup(&client, &[("a", "b")]).await.unwrap(), vec![item]);
}

#[tokio::test]
async fn applications_are_attributed_isolated_and_revocable() {
    let world = World::new(true);
    let chromium = world.client("binary:/usr/bin/chromium").await;
    let other = world.client("binary:/usr/bin/1password").await;
    let tool = world.client("local-process:/home/me/tool").await;
    let settings = world.client("binary:/usr/bin/tilo-secret-service").await;

    let (session, key) = dh_session(&chromium).await;
    let item = store_password(
        &chromium,
        &session,
        &key,
        "c",
        &[("app", "shared-name")],
        b"chromium-secret",
    )
    .await;

    // The same query from another application finds nothing of Chromium's,
    // and its path alone opens nothing.
    let (other_session, other_key) = dh_session(&other).await;
    assert!(
        lookup(&other, &[("app", "shared-name")])
            .await
            .unwrap()
            .is_empty()
    );
    let foreign = proxy(&other, item.as_str(), ITEM).await;
    assert!(
        foreign
            .call::<_, _, (WireSecret,)>("GetSecret", &(&other_session,))
            .await
            .is_err()
    );
    assert!(
        foreign
            .call::<_, _, OwnedObjectPath>("Delete", &())
            .await
            .is_err()
    );
    let theirs = store_password(
        &other,
        &other_session,
        &other_key,
        "o",
        &[("app", "shared-name")],
        b"other",
    )
    .await;
    assert_ne!(theirs, item);
    assert_eq!(
        read_password(&chromium, &session, &key, &item).await,
        b"chromium-secret"
    );

    // Property reads are attributed as well: the other application lists no
    // item of Chromium's under `Items`, and cannot read what the item says.
    let other_collection = proxy(&other, DEFAULT, COLLECTION).await;
    assert_eq!(
        other_collection
            .get_property::<Vec<OwnedObjectPath>>("Items")
            .await
            .unwrap(),
        vec![theirs.clone()]
    );
    for property in ["Label", "Created", "Modified"] {
        assert!(
            foreign.get_property::<OwnedValue>(property).await.is_err(),
            "{property}"
        );
    }
    assert!(
        foreign
            .get_property::<HashMap<String, String>>("Attributes")
            .await
            .is_err()
    );
    let own = proxy(&other, theirs.as_str(), ITEM).await;
    assert_eq!(own.get_property::<String>("Label").await.unwrap(), "o");

    // An application nobody installed is recorded as such.
    let (tool_session, tool_key) = dh_session(&tool).await;
    store_password(&tool, &tool_session, &tool_key, "t", &[("app", "t")], b"t").await;

    // Settings lists who used the store, by identity; others cannot.
    let management = proxy(&settings, MANAGEMENT_PATH, "so.tilo.Secrets1").await;
    let rows: Vec<(String, String, String, u64, u64, u64, u32)> =
        management.call("ListApps", &()).await.unwrap();
    let ids: Vec<&str> = rows.iter().map(|row| row.0.as_str()).collect();
    assert!(ids.contains(&"binary:/usr/bin/chromium"));
    assert!(ids.contains(&"binary:/usr/bin/1password"));
    assert!(ids.contains(&"local-process:/home/me/tool"));
    let chromium_row = rows
        .iter()
        .find(|row| row.0 == "binary:/usr/bin/chromium")
        .unwrap();
    assert_eq!((chromium_row.2.as_str(), chromium_row.6), ("allowed", 1));
    let tool_row = rows
        .iter()
        .find(|row| row.0 == "local-process:/home/me/tool")
        .unwrap();
    assert!(tool_row.1.contains("not part of the system"));
    let unprivileged = proxy(&tool, MANAGEMENT_PATH, "so.tilo.Secrets1").await;
    assert!(
        unprivileged
            .call::<_, _, Vec<(String, String, String, u64, u64, u64, u32)>>("ListApps", &())
            .await
            .is_err()
    );
    assert!(
        unprivileged
            .call::<_, _, ()>("SetAppAccess", &("binary:/usr/bin/chromium", false))
            .await
            .is_err()
    );

    // Revoking Chromium denies it everything and leaves the others alone.
    management
        .call::<_, _, ()>("SetAppAccess", &("binary:/usr/bin/chromium", false))
        .await
        .unwrap();
    let denied = lookup(&chromium, &[("app", "shared-name")])
        .await
        .unwrap_err();
    assert_eq!(
        error_name(&denied),
        "org.freedesktop.Secret.Error.AccessDenied"
    );
    assert!(lookup(&other, &[("app", "shared-name")]).await.is_ok());
    management
        .call::<_, _, ()>("SetAppAccess", &("binary:/usr/bin/chromium", true))
        .await
        .unwrap();
    assert_eq!(
        lookup(&chromium, &[("app", "shared-name")]).await.unwrap(),
        vec![item]
    );
}

#[tokio::test]
async fn the_portal_host_gets_sandbox_secrets_from_the_same_store() {
    let world = World::new(true);
    let shell = world.client("binary:/usr/bin/shell").await;
    let settings = world.client("binary:/usr/bin/tilo-secret-service").await;
    let intruder = world.client("local-process:/home/me/tool").await;

    let portal = proxy(&shell, MANAGEMENT_PATH, "so.tilo.Secrets1").await;
    let one: Vec<u8> = portal
        .call("SandboxSecret", &("so.tilo.notes", Vec::<u8>::new()))
        .await
        .unwrap();
    assert_eq!(one.len(), 32);
    assert_eq!(
        one,
        portal
            .call::<_, _, Vec<u8>>("SandboxSecret", &("so.tilo.notes", Vec::<u8>::new()))
            .await
            .unwrap()
    );
    assert_ne!(
        one,
        portal
            .call::<_, _, Vec<u8>>("SandboxSecret", &("so.tilo.chat", Vec::<u8>::new()))
            .await
            .unwrap()
    );
    // A secret from the portal's earlier store is adopted for an app the
    // vault has not seen.
    let adopted: Vec<u8> = portal
        .call("SandboxSecret", &("so.tilo.old", vec![7_u8; 32]))
        .await
        .unwrap();
    assert_eq!(adopted, vec![7_u8; 32]);
    // Only the portal host may ask; Settings and strangers may not.
    for refused in [&settings, &intruder] {
        let management = proxy(refused, MANAGEMENT_PATH, "so.tilo.Secrets1").await;
        assert!(
            management
                .call::<_, _, Vec<u8>>("SandboxSecret", &("so.tilo.notes", Vec::<u8>::new()))
                .await
                .is_err()
        );
    }

    // The sandboxed app is an application like any other: listed, and
    // revocable in Settings.
    let management = proxy(&settings, MANAGEMENT_PATH, "so.tilo.Secrets1").await;
    let rows: Vec<(String, String, String, u64, u64, u64, u32)> =
        management.call("ListApps", &()).await.unwrap();
    assert!(
        rows.iter()
            .any(|row| row.0 == "sandbox:so.tilo.notes" && row.2 == "allowed")
    );
    management
        .call::<_, _, ()>("SetAppAccess", &("sandbox:so.tilo.notes", false))
        .await
        .unwrap();
    assert!(
        portal
            .call::<_, _, Vec<u8>>("SandboxSecret", &("so.tilo.notes", Vec::<u8>::new()))
            .await
            .is_err()
    );
    // Forgetting it discards its secret: the next one is new.
    management
        .call::<_, _, ()>("ForgetApp", &("sandbox:so.tilo.notes",))
        .await
        .unwrap();
    let fresh: Vec<u8> = portal
        .call("SandboxSecret", &("so.tilo.notes", Vec::<u8>::new()))
        .await
        .unwrap();
    assert_ne!(fresh, one);
}

#[tokio::test]
async fn items_survive_a_restart_of_the_service() {
    let world = World::new(true);
    let first = world.client("binary:/usr/bin/chromium").await;
    let (session, key) = dh_session(&first).await;
    store_password(&first, &session, &key, "c", &[("k", "v")], b"kept").await;

    // A new service process over the same files.
    let store = Arc::new(Store::new(
        world.dir.path().join("vault"),
        Box::new(world.device.clone()),
    ));
    let core = Core::new(
        store,
        Arc::new(Identity(app("binary:/usr/bin/chromium"))),
        world.managers.clone(),
    );
    let (server_end, client_end) = tokio::net::UnixStream::pair().unwrap();
    let server = Builder::unix_stream(server_end)
        .server(zbus::Guid::generate())
        .unwrap()
        .p2p()
        .build();
    let client = Builder::unix_stream(client_end).p2p().build();
    let (server, client) = futures_util::try_join!(server, client).unwrap();
    publish(&server, &core).await.unwrap();
    let (session, key) = dh_session(&client).await;
    let found = lookup(&client, &[("k", "v")]).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(
        read_password(&client, &session, &key, &found[0]).await,
        b"kept"
    );
    drop(server);
}
