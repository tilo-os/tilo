//! The D-Bus face of the store: `org.freedesktop.Secret.*` as libsecret
//! speaks it, plus the small `so.tilo.Secrets1` management interface that
//! Settings and the desktop portal use.
//!
//! The service never prompts. A store whose identity is unavailable answers
//! `IsLocked` and `Unlock` simply reports nothing unlocked; there is no
//! unlock dialog to loop on, because no secret the person types opens it.
//!
//! Every method attributes its caller (see [`CallerResolver`]) and the
//! store answers for that caller alone: another application's items are not
//! found, not merely refused. D-Bus property reads carry no caller, so an
//! item's `Label` and `Attributes` are readable by whoever knows its path;
//! the path holds 128 random bits and is only ever returned to callers
//! the item is visible to, and no secret is reachable through a property.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_seal::secret_service::{DH_ALGORITHM, Handshake, TransportKey};
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Type, Value};
use zbus::{Connection, ObjectServer, interface};
use zeroize::Zeroizing;

use crate::app::{AppId, UnknownCaller, identify_process, proc_root, trusted_managers};
use crate::store::{ItemRef, NewItem, Store, StoreError};
use crate::vault::{Access, LOGIN_COLLECTION, SESSION_COLLECTION};

pub const BUS_NAME: &str = "org.freedesktop.secrets";
pub const ROOT: &str = "/org/freedesktop/secrets";
pub const MANAGEMENT_PATH: &str = "/so/tilo/Secrets1";
const NO_PROMPT: &str = "/";
const ITEM_LABEL: &str = "org.freedesktop.Secret.Item.Label";
const ITEM_ATTRIBUTES: &str = "org.freedesktop.Secret.Item.Attributes";
const COLLECTION_LABEL: &str = "org.freedesktop.Secret.Collection.Label";

/// How a caller of the service is identified.
pub trait CallerResolver: Send + Sync + 'static {
    /// The identity behind the D-Bus `sender` (a unique name; `None` on a
    /// peer-to-peer connection, which has no bus).
    fn identify<'a>(
        &'a self,
        sender: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<AppId, UnknownCaller>> + Send + 'a>>;
}

/// Identity from the session bus: the daemon reports the sender's user and
/// process, and the process is examined under `/proc`.
pub struct BusResolver {
    connection: Connection,
    uid: u32,
}

impl BusResolver {
    #[must_use]
    pub fn new(connection: Connection, uid: u32) -> Self {
        Self { connection, uid }
    }
}

impl CallerResolver for BusResolver {
    fn identify<'a>(
        &'a self,
        sender: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<AppId, UnknownCaller>> + Send + 'a>> {
        Box::pin(async move {
            let name = zbus::names::BusName::try_from(sender.ok_or(UnknownCaller::Unreadable)?)
                .map_err(|_| UnknownCaller::Unreadable)?;
            let bus = zbus::fdo::DBusProxy::new(&self.connection)
                .await
                .map_err(|_| UnknownCaller::Unreadable)?;
            let credentials = bus
                .get_connection_credentials(name)
                .await
                .map_err(|_| UnknownCaller::Unreadable)?;
            if credentials.unix_user_id() != Some(self.uid) {
                return Err(UnknownCaller::OtherUser);
            }
            let pid = credentials.process_id().ok_or(UnknownCaller::Unreadable)?;
            identify_process(&proc_root(), pid, self.uid)
        })
    }
}

/// Which callers may use the management interface.
#[derive(Clone, Debug)]
pub struct Managers {
    /// May list, revoke and forget applications.
    pub settings: Vec<AppId>,
    /// May ask for a sandboxed app's master secret on its behalf.
    pub portal: Vec<AppId>,
}

impl Managers {
    /// The programs of the image: the compositor hosts the portal, and it
    /// and this binary's command line serve Settings.
    #[must_use]
    pub fn system() -> Self {
        let settings = trusted_managers();
        let portal = AppId::parse("binary:/usr/bin/shell").into_iter().collect();
        Self { settings, portal }
    }
}

/// What a session encrypts with.
enum SessionKey {
    Plain,
    Dh(TransportKey),
}

struct SessionEntry {
    /// The bus name that opened it; no other caller may use it.
    owner: String,
    key: SessionKey,
}

/// Everything the interface objects share.
pub struct Core {
    store: Arc<Store>,
    resolver: Arc<dyn CallerResolver>,
    managers: Managers,
    sessions: Mutex<HashMap<String, SessionEntry>>,
    next_session: AtomicU64,
}

/// The `(oayays)` secret of the API.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct WireSecret(pub OwnedObjectPath, pub Vec<u8>, pub Vec<u8>, pub String);

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.Secret.Error")]
pub enum SecretError {
    #[zbus(error)]
    ZBus(zbus::Error),
    IsLocked(String),
    NoSession(String),
    NoSuchObject(String),
    AccessDenied(String),
    InvalidArgs(String),
    Failed(String),
}

type Result<T> = std::result::Result<T, SecretError>;

impl From<StoreError> for SecretError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Locked(locked) => Self::IsLocked(locked.to_string()),
            StoreError::Revoked => Self::AccessDenied(error.to_string()),
            StoreError::NoSuchObject => Self::NoSuchObject(error.to_string()),
            StoreError::Invalid(_) => Self::InvalidArgs(error.to_string()),
            StoreError::Io(_) => Self::Failed(error.to_string()),
        }
    }
}

impl From<UnknownCaller> for SecretError {
    fn from(error: UnknownCaller) -> Self {
        Self::AccessDenied(error.to_string())
    }
}

fn sender_of(header: &Header<'_>) -> Option<String> {
    header.sender().map(ToString::to_string)
}

fn collection_path(name: &str) -> String {
    format!("{ROOT}/collection/{name}")
}

fn alias_path(alias: &str) -> String {
    format!("{ROOT}/aliases/{alias}")
}

fn item_path(item: &ItemRef) -> String {
    format!("{ROOT}/collection/{}/{}", item.collection, item.id)
}

fn object(path: &str) -> Result<OwnedObjectPath> {
    OwnedObjectPath::try_from(path).map_err(|_| SecretError::Failed("object path".into()))
}

/// The collection or alias name in `path`.
fn parse_collection(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(ROOT)?.strip_prefix('/')?;
    let (kind, name) = rest.split_once('/')?;
    ((kind == "collection" || kind == "aliases") && !name.is_empty() && !name.contains('/'))
        .then_some(name)
}

fn parse_item(path: &str) -> Option<ItemRef> {
    let rest = path.strip_prefix(ROOT)?.strip_prefix("/collection/")?;
    let (collection, id) = rest.split_once('/')?;
    (!id.is_empty() && !id.contains('/')).then(|| ItemRef {
        collection: collection.to_owned(),
        id: id.to_owned(),
    })
}

impl Core {
    #[must_use]
    pub fn new(
        store: Arc<Store>,
        resolver: Arc<dyn CallerResolver>,
        managers: Managers,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            resolver,
            managers,
            sessions: Mutex::new(HashMap::new()),
            next_session: AtomicU64::new(1),
        })
    }

    async fn caller(&self, header: &Header<'_>) -> Result<AppId> {
        let sender = sender_of(header);
        Ok(self.resolver.identify(sender.as_deref()).await?)
    }

    async fn manager(&self, header: &Header<'_>, allowed: &[AppId]) -> Result<AppId> {
        let caller = self.caller(header).await?;
        allowed
            .contains(&caller)
            .then_some(caller)
            .ok_or_else(|| SecretError::AccessDenied("not a manager of the secret store".into()))
    }

    fn sessions(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, SessionEntry>>> {
        self.sessions
            .lock()
            .map_err(|_| SecretError::Failed("session table poisoned".into()))
    }

    /// Forget every session opened by a bus name that went away.
    pub fn drop_sessions_of(&self, owner: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.retain(|_, session| session.owner != owner);
        }
    }

    fn encrypt(&self, session: &str, sender: &str, plain: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let sessions = self.sessions()?;
        let entry = sessions
            .get(session)
            .filter(|entry| entry.owner == sender)
            .ok_or_else(|| SecretError::NoSession("no such session".into()))?;
        match &entry.key {
            SessionKey::Plain => Ok((Vec::new(), plain.to_vec())),
            SessionKey::Dh(key) => {
                let (iv, ciphertext) = key
                    .encrypt(plain)
                    .map_err(|error| SecretError::Failed(error.to_string()))?;
                Ok((iv.to_vec(), ciphertext))
            }
        }
    }

    fn decrypt(&self, sender: &str, secret: &WireSecret) -> Result<Zeroizing<Vec<u8>>> {
        let sessions = self.sessions()?;
        let entry = sessions
            .get(secret.0.as_str())
            .filter(|entry| entry.owner == sender)
            .ok_or_else(|| SecretError::NoSession("no such session".into()))?;
        match &entry.key {
            SessionKey::Plain => Ok(Zeroizing::new(secret.2.clone())),
            SessionKey::Dh(key) => key
                .decrypt(&secret.1, &secret.2)
                .map_err(|_| SecretError::InvalidArgs("the secret does not decrypt".into())),
        }
    }

    fn wire_secret(
        &self,
        session: &ObjectPath<'_>,
        sender: &str,
        item: &ItemRef,
        caller: &AppId,
    ) -> Result<WireSecret> {
        let (secret, content_type) = self.store.read_secret(caller, item)?;
        let (parameters, value) = self.encrypt(session.as_str(), sender, secret.expose())?;
        Ok(WireSecret(
            object(session.as_str())?,
            parameters,
            value,
            content_type,
        ))
    }
}

/// Register every object the store has today, and the two collections and
/// aliases that must answer even while it is locked.
///
/// # Errors
///
/// Any failure of the object server.
pub async fn publish(connection: &Connection, core: &Arc<Core>) -> zbus::Result<()> {
    let server = connection.object_server();
    server
        .at(
            ROOT,
            ServiceIface {
                core: Arc::clone(core),
            },
        )
        .await?;
    server
        .at(
            MANAGEMENT_PATH,
            ManagementIface {
                core: Arc::clone(core),
            },
        )
        .await?;
    let mut names = vec![LOGIN_COLLECTION.to_owned(), SESSION_COLLECTION.to_owned()];
    if let Ok(collections) = core.store.collections() {
        names.extend(collections.into_iter().map(|(name, _)| name));
    }
    names.sort();
    names.dedup();
    for name in &names {
        publish_collection(server, core, name).await?;
    }
    for alias in ["default", SESSION_COLLECTION] {
        server
            .at(
                alias_path(alias),
                CollectionIface {
                    core: Arc::clone(core),
                    name: alias.to_owned(),
                },
            )
            .await?;
    }
    Ok(())
}

async fn publish_collection(
    server: &ObjectServer,
    core: &Arc<Core>,
    name: &str,
) -> zbus::Result<()> {
    server
        .at(
            collection_path(name),
            CollectionIface {
                core: Arc::clone(core),
                name: name.to_owned(),
            },
        )
        .await?;
    Ok(())
}

async fn publish_item(server: &ObjectServer, core: &Arc<Core>, item: &ItemRef) -> zbus::Result<()> {
    server
        .at(
            item_path(item),
            ItemIface {
                core: Arc::clone(core),
                item: item.clone(),
            },
        )
        .await?;
    Ok(())
}

// ---- org.freedesktop.Secret.Service ------------------------------------

struct ServiceIface {
    core: Arc<Core>,
}

fn property_text(properties: &HashMap<String, OwnedValue>, key: &str) -> Result<Option<String>> {
    properties
        .get(key)
        .map(|value| {
            let value = value
                .try_clone()
                .map_err(|_| SecretError::InvalidArgs(format!("{key} must be a string")))?;
            String::try_from(value)
                .map_err(|_| SecretError::InvalidArgs(format!("{key} must be a string")))
        })
        .transpose()
}

#[interface(name = "org.freedesktop.Secret.Service")]
impl ServiceIface {
    async fn open_session(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        algorithm: String,
        input: Value<'_>,
    ) -> zbus::fdo::Result<(OwnedValue, OwnedObjectPath)> {
        let failed = |error: &dyn std::fmt::Display| zbus::fdo::Error::Failed(error.to_string());
        let (output, key) = match algorithm.as_str() {
            "plain" => (
                OwnedValue::try_from(Value::from("")).map_err(|e| failed(&e))?,
                SessionKey::Plain,
            ),
            DH_ALGORITHM => {
                let peer = Vec::<u8>::try_from(input).map_err(|_| {
                    zbus::fdo::Error::InvalidArgs("the public value must be bytes".into())
                })?;
                let handshake = Handshake::new().map_err(|e| failed(&e))?;
                let public = handshake.public().to_vec();
                let key = handshake
                    .finish(&peer)
                    .map_err(|_| zbus::fdo::Error::InvalidArgs("invalid public value".into()))?;
                (
                    OwnedValue::try_from(Value::from(public)).map_err(|e| failed(&e))?,
                    SessionKey::Dh(key),
                )
            }
            other => {
                return Err(zbus::fdo::Error::NotSupported(format!(
                    "session algorithm {other}"
                )));
            }
        };
        let number = self.core.next_session.fetch_add(1, Ordering::Relaxed);
        let path = format!("{ROOT}/session/s{number}");
        let owner = sender_of(&header).unwrap_or_default();
        self.core
            .sessions
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("session table poisoned".into()))?
            .insert(path.clone(), SessionEntry { owner, key });
        server
            .at(
                path.as_str(),
                SessionIface {
                    core: Arc::clone(&self.core),
                    path: path.clone(),
                },
            )
            .await?;
        let path = OwnedObjectPath::try_from(path).map_err(|e| failed(&e))?;
        Ok((output, path))
    }

    async fn create_collection(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        properties: HashMap<String, OwnedValue>,
        alias: String,
    ) -> Result<(OwnedObjectPath, OwnedObjectPath)> {
        let caller = self.core.caller(&header).await?;
        let label = property_text(&properties, COLLECTION_LABEL)?.unwrap_or_default();
        let name = self.core.store.create_collection(
            &caller,
            &label,
            (!alias.is_empty()).then_some(alias.as_str()),
        )?;
        publish_collection(server, &self.core, &name).await?;
        Ok((object(&collection_path(&name))?, object(NO_PROMPT)?))
    }

    async fn search_items(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        attributes: HashMap<String, String>,
    ) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>)> {
        let caller = self.core.caller(&header).await?;
        let query: BTreeMap<_, _> = attributes.into_iter().collect();
        let found = self.core.store.search(&caller, None, &query);
        match found {
            Ok(items) => {
                let mut paths = Vec::with_capacity(items.len());
                for item in &items {
                    publish_item(server, &self.core, item).await?;
                    paths.push(object(&item_path(item))?);
                }
                Ok((paths, Vec::new()))
            }
            // Locked: nothing can be listed, so nothing is reported either
            // way; the caller learns it from the error.
            Err(error) => Err(error.into()),
        }
    }

    async fn unlock(
        &self,
        objects: Vec<OwnedObjectPath>,
    ) -> Result<(Vec<OwnedObjectPath>, OwnedObjectPath)> {
        // The key comes from the device identity, never from a prompt: the
        // objects are unlocked exactly when the store is open.
        let unlocked = if self.core.store.locked().is_none() {
            objects
        } else {
            Vec::new()
        };
        Ok((unlocked, object(NO_PROMPT)?))
    }

    async fn lock(
        &self,
        _objects: Vec<OwnedObjectPath>,
    ) -> Result<(Vec<OwnedObjectPath>, OwnedObjectPath)> {
        // The key is not the person's to withdraw: it is released again at
        // the next operation, so locking would only pretend.
        Ok((Vec::new(), object(NO_PROMPT)?))
    }

    async fn get_secrets(
        &self,
        #[zbus(header)] header: Header<'_>,
        items: Vec<OwnedObjectPath>,
        session: ObjectPath<'_>,
    ) -> Result<HashMap<OwnedObjectPath, WireSecret>> {
        let caller = self.core.caller(&header).await?;
        let sender = sender_of(&header).unwrap_or_default();
        let mut out = HashMap::new();
        for path in items {
            let Some(item) = parse_item(path.as_str()) else {
                continue;
            };
            match self.core.wire_secret(&session, &sender, &item, &caller) {
                Ok(secret) => {
                    out.insert(path, secret);
                }
                // Not visible to this caller: absent from the answer, as
                // for an item that does not exist.
                Err(SecretError::NoSuchObject(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(out)
    }

    async fn read_alias(&self, name: String) -> Result<OwnedObjectPath> {
        match self.core.store.read_alias(&name) {
            Ok(_) => Ok(object(&alias_path(&name))?),
            // Even while locked, the two aliases libsecret uses exist.
            Err(StoreError::Locked(_)) if name == "default" || name == SESSION_COLLECTION => {
                Ok(object(&alias_path(&name))?)
            }
            Err(StoreError::NoSuchObject) => Ok(object(NO_PROMPT)?),
            Err(error) => Err(error.into()),
        }
    }

    async fn set_alias(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        name: String,
        collection: ObjectPath<'_>,
    ) -> Result<()> {
        let caller = self.core.caller(&header).await?;
        let target = if collection.as_str() == "/" {
            return Err(SecretError::InvalidArgs("aliases cannot be removed".into()));
        } else {
            parse_collection(collection.as_str())
                .ok_or_else(|| SecretError::NoSuchObject("not a collection".into()))?
        };
        let resolved = self.core.store.read_alias(target)?;
        self.core.store.set_alias(&caller, &name, &resolved)?;
        server
            .at(
                alias_path(&name),
                CollectionIface {
                    core: Arc::clone(&self.core),
                    name: name.clone(),
                },
            )
            .await
            .map_err(|e| SecretError::Failed(e.to_string()))?;
        Ok(())
    }

    #[zbus(property)]
    async fn collections(&self) -> Vec<OwnedObjectPath> {
        self.core
            .store
            .collections()
            .map(|all| {
                all.iter()
                    .filter_map(|(name, _)| object(&collection_path(name)).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[zbus(signal)]
    async fn collection_created(
        emitter: &SignalEmitter<'_>,
        collection: ObjectPath<'_>,
    ) -> zbus::Result<()>;
}

// ---- org.freedesktop.Secret.Session ------------------------------------

struct SessionIface {
    core: Arc<Core>,
    path: String,
}

#[interface(name = "org.freedesktop.Secret.Session")]
impl SessionIface {
    async fn close(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<()> {
        let sender = sender_of(&header).unwrap_or_default();
        {
            let mut sessions = self.core.sessions()?;
            if sessions
                .get(&self.path)
                .is_none_or(|entry| entry.owner != sender)
            {
                return Err(SecretError::NoSession("no such session".into()));
            }
            sessions.remove(&self.path);
        }
        server
            .remove::<SessionIface, _>(self.path.as_str())
            .await
            .map_err(|e| SecretError::Failed(e.to_string()))?;
        Ok(())
    }
}

// ---- org.freedesktop.Secret.Collection ---------------------------------

struct CollectionIface {
    core: Arc<Core>,
    /// A collection name or an alias; resolved on every call.
    name: String,
}

impl CollectionIface {
    fn canonical(&self) -> Result<String> {
        Ok(self.core.store.read_alias(&self.name)?)
    }
}

#[interface(name = "org.freedesktop.Secret.Collection")]
impl CollectionIface {
    async fn delete(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> Result<OwnedObjectPath> {
        let caller = self.core.caller(&header).await?;
        let name = self.canonical()?;
        self.core.store.delete_collection(&caller, &name)?;
        server
            .remove::<CollectionIface, _>(collection_path(&name))
            .await
            .map_err(|e| SecretError::Failed(e.to_string()))?;
        object(NO_PROMPT)
    }

    async fn search_items(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        attributes: HashMap<String, String>,
    ) -> Result<Vec<OwnedObjectPath>> {
        let caller = self.core.caller(&header).await?;
        let query: BTreeMap<_, _> = attributes.into_iter().collect();
        let items = self
            .core
            .store
            .search(&caller, Some(&self.canonical()?), &query)?;
        let mut paths = Vec::with_capacity(items.len());
        for item in &items {
            publish_item(server, &self.core, item).await?;
            paths.push(object(&item_path(item))?);
        }
        Ok(paths)
    }

    async fn create_item(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] connection: &Connection,
        properties: HashMap<String, OwnedValue>,
        secret: WireSecret,
        replace: bool,
    ) -> Result<(OwnedObjectPath, OwnedObjectPath)> {
        let caller = self.core.caller(&header).await?;
        let sender = sender_of(&header).unwrap_or_default();
        let label = property_text(&properties, ITEM_LABEL)?.unwrap_or_default();
        let content_type = secret.3.clone();
        let attributes = properties
            .get(ITEM_ATTRIBUTES)
            .map(|value| {
                let value = value
                    .try_clone()
                    .map_err(|_| SecretError::InvalidArgs("Attributes must be a{ss}".into()))?;
                HashMap::<String, String>::try_from(value)
                    .map_err(|_| SecretError::InvalidArgs("Attributes must be a{ss}".into()))
            })
            .transpose()?
            .unwrap_or_default();
        let plain = self.core.decrypt(&sender, &secret)?;
        let created = self.core.store.create_item(
            &caller,
            &self.name,
            NewItem {
                label,
                attributes: attributes.into_iter().collect(),
                secret: plain.to_vec(),
                content_type: if content_type.is_empty() {
                    "text/plain".into()
                } else {
                    content_type
                },
            },
            replace,
        )?;
        publish_item(server, &self.core, &created).await?;
        let collection = collection_path(&created.collection);
        let path = item_path(&created);
        let emitter = SignalEmitter::new(connection, collection.as_str())?;
        let _ = Self::item_created(&emitter, object(&path)?.as_ref()).await;
        Ok((object(&path)?, object(NO_PROMPT)?))
    }

    #[zbus(property)]
    async fn items(&self) -> Vec<OwnedObjectPath> {
        // A property read names no caller, so it must not list items.
        Vec::new()
    }

    #[zbus(property)]
    async fn label(&self) -> String {
        self.core
            .store
            .collection_info(&self.name)
            .map(|(label, _, _)| label)
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn locked(&self) -> bool {
        self.core.store.locked().is_some()
    }

    #[zbus(property)]
    async fn created(&self) -> u64 {
        self.core
            .store
            .collection_info(&self.name)
            .map_or(0, |(_, created, _)| created)
    }

    #[zbus(property)]
    async fn modified(&self) -> u64 {
        self.core
            .store
            .collection_info(&self.name)
            .map_or(0, |(_, _, modified)| modified)
    }

    #[zbus(signal)]
    async fn item_created(emitter: &SignalEmitter<'_>, item: ObjectPath<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn item_deleted(emitter: &SignalEmitter<'_>, item: ObjectPath<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn item_changed(emitter: &SignalEmitter<'_>, item: ObjectPath<'_>) -> zbus::Result<()>;
}

// ---- org.freedesktop.Secret.Item ---------------------------------------

struct ItemIface {
    core: Arc<Core>,
    item: ItemRef,
}

impl ItemIface {
    fn info(&self) -> Option<crate::store::ItemInfo> {
        self.core.store.item_info(&self.item).ok()
    }
}

#[interface(name = "org.freedesktop.Secret.Item")]
impl ItemIface {
    async fn delete(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] connection: &Connection,
    ) -> Result<OwnedObjectPath> {
        let caller = self.core.caller(&header).await?;
        self.core.store.delete_item(&caller, &self.item)?;
        let path = item_path(&self.item);
        server
            .remove::<ItemIface, _>(path.as_str())
            .await
            .map_err(|e| SecretError::Failed(e.to_string()))?;
        let emitter = SignalEmitter::new(connection, collection_path(&self.item.collection))?;
        let _ = CollectionIface::item_deleted(&emitter, object(&path)?.as_ref()).await;
        object(NO_PROMPT)
    }

    async fn get_secret(
        &self,
        #[zbus(header)] header: Header<'_>,
        session: ObjectPath<'_>,
    ) -> Result<WireSecret> {
        let caller = self.core.caller(&header).await?;
        let sender = sender_of(&header).unwrap_or_default();
        self.core
            .wire_secret(&session, &sender, &self.item, &caller)
    }

    async fn set_secret(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        secret: WireSecret,
    ) -> Result<()> {
        let caller = self.core.caller(&header).await?;
        let sender = sender_of(&header).unwrap_or_default();
        let plain = self.core.decrypt(&sender, &secret)?;
        self.core
            .store
            .write_secret(&caller, &self.item, plain.to_vec(), secret.3.clone())?;
        let emitter = SignalEmitter::new(connection, collection_path(&self.item.collection))?;
        let _ =
            CollectionIface::item_changed(&emitter, object(&item_path(&self.item))?.as_ref()).await;
        Ok(())
    }

    #[zbus(property)]
    async fn locked(&self) -> bool {
        self.core.store.locked().is_some()
    }

    #[zbus(property)]
    async fn attributes(&self) -> HashMap<String, String> {
        self.info()
            .map(|info| info.attributes.into_iter().collect())
            .unwrap_or_default()
    }

    #[zbus(property)]
    async fn label(&self) -> String {
        self.info().map(|info| info.label).unwrap_or_default()
    }

    #[zbus(property)]
    async fn created(&self) -> u64 {
        self.info().map_or(0, |info| info.created)
    }

    #[zbus(property)]
    async fn modified(&self) -> u64 {
        self.info().map_or(0, |info| info.modified)
    }
}

// ---- so.tilo.Secrets1 -----------------------------------------------------

/// One application as the management interface lists it:
/// `(identity, name, access, first seen, last used, uses, items)`.
pub type AppRow = (String, String, String, u64, u64, u64, u32);

struct ManagementIface {
    core: Arc<Core>,
}

fn access_name(access: Access) -> &'static str {
    match access {
        Access::Allowed => "allowed",
        Access::Revoked => "revoked",
    }
}

fn parse_identity(text: &str) -> Result<AppId> {
    AppId::parse(text).ok_or_else(|| SecretError::InvalidArgs("not an application identity".into()))
}

#[interface(name = "so.tilo.Secrets1")]
impl ManagementIface {
    /// `(locked, reason, imported from an older keyring)`.
    async fn status(&self) -> (bool, String, String) {
        let locked = self.core.store.locked();
        let migration = self
            .core
            .store
            .migration()
            .ok()
            .flatten()
            .map(|done| done.source)
            .unwrap_or_default();
        (
            locked.is_some(),
            locked.map(|why| why.to_string()).unwrap_or_default(),
            migration,
        )
    }

    async fn list_apps(&self, #[zbus(header)] header: Header<'_>) -> Result<Vec<AppRow>> {
        self.core
            .manager(&header, &self.core.managers.settings)
            .await?;
        Ok(self
            .core
            .store
            .apps()?
            .into_iter()
            .map(|app| {
                (
                    app.id.to_string(),
                    app.name,
                    access_name(app.access).to_owned(),
                    app.first_seen,
                    app.last_used,
                    app.uses,
                    app.items,
                )
            })
            .collect())
    }

    async fn set_app_access(
        &self,
        #[zbus(header)] header: Header<'_>,
        app: String,
        allowed: bool,
    ) -> Result<()> {
        self.core
            .manager(&header, &self.core.managers.settings)
            .await?;
        let access = if allowed {
            Access::Allowed
        } else {
            Access::Revoked
        };
        Ok(self.core.store.set_access(&parse_identity(&app)?, access)?)
    }

    async fn forget_app(&self, #[zbus(header)] header: Header<'_>, app: String) -> Result<()> {
        self.core
            .manager(&header, &self.core.managers.settings)
            .await?;
        Ok(self.core.store.forget_app(&parse_identity(&app)?)?)
    }

    /// The master secret of the sandboxed app `package`, for the portal
    /// host to hand to that app.
    async fn sandbox_secret(
        &self,
        #[zbus(header)] header: Header<'_>,
        package: String,
    ) -> Result<Vec<u8>> {
        self.core
            .manager(&header, &self.core.managers.portal)
            .await?;
        Ok(self.core.store.sandbox_secret(&package)?.to_vec())
    }
}
