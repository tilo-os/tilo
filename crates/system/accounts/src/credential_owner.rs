//! One host-owned vault/broker for a person on one SDK device.
use super::{
    BrokerError, CredentialBroker, CredentialGrant, CredentialScope, SecretId, SecretItem,
    SecretItemKind, SecretStore,
};
use std::sync::{Arc, Mutex, OnceLock};

/// The native host lends this vault view only to the matching Passwords app.
/// Messaging receives its own restricted capability, never this owner handle.
/// Creating a deferred owner neither opens storage nor asks for a credential.
#[derive(Clone)]
pub struct CredentialOwner {
    principal: String,
    device: String,
    broker: Arc<OnceLock<Arc<Mutex<CredentialBroker>>>>,
    open: Option<Arc<dyn Fn() -> SecretStore + Send + Sync>>,
    pub(super) requests: Arc<super::pending::PendingRequests>,
}
impl CredentialOwner {
    /// Prepare a new private profile key in this original system vault. This
    /// creates no credential permission. An existing scope is refused rather
    /// than re-keyed; callers must use their retained existing item instead.
    /// # Errors
    /// Refuses a foreign scope, weaker custody, duplicate scope or failed save.
    pub fn prepare_profile_key(
        &self,
        scope: &CredentialScope,
        origin: &str,
    ) -> Result<SecretId, BrokerError> {
        use getrandom::{SysRng, rand_core::TryRng as _};
        use std::fmt::Write as _;
        use zeroize::{Zeroize as _, Zeroizing};
        if scope.principal != self.principal || scope.device != self.device || !scope.valid() {
            return Err(BrokerError::Denied);
        }
        let origin = super::canonical_origin(origin)?;
        let encoded_scope = serde_json::to_string(scope).map_err(|_| BrokerError::CorruptItem)?;
        self.edit(|store| {
            if store.custody() != crate::SecretCustody::System || store.recovery_error().is_some() {
                return Err(BrokerError::Denied);
            }
            let mut occupied = false;
            for mut item in store.list() {
                occupied |= item.fields.get("tilo.messaging.profile.scope") == Some(&encoded_scope);
                item.password.zeroize();
            }
            if occupied {
                return Err(BrokerError::Denied);
            }
            let mut bytes = Zeroizing::new([0u8; 32]);
            SysRng
                .try_fill_bytes(bytes.as_mut())
                .map_err(|_| BrokerError::Randomness)?;
            let mut encoded = Zeroizing::new(String::with_capacity(64));
            for byte in bytes.iter() {
                write!(encoded, "{byte:02x}").map_err(|_| BrokerError::Randomness)?;
            }
            let mut item =
                SecretItem::login("Messages account encryption key", "", encoded.as_str());
            item.kind = SecretItemKind::Other;
            item.origins.insert(origin);
            item.fields
                .insert("tilo.messaging.profile.scope".into(), encoded_scope);
            let saved = store.put(&mut item);
            item.password.zeroize();
            saved?;
            Ok(item.id)
        })?
    }
    /// Prepare the original native vault lazily, without opening/provisioning it.
    /// # Errors
    /// Refuses malformed person or SDK-device labels.
    pub fn deferred(
        principal: &str,
        device: &str,
        open: impl Fn() -> SecretStore + Send + Sync + 'static,
    ) -> Result<Self, BrokerError> {
        Self::validate(principal, device)?;
        Ok(Self {
            principal: principal.into(),
            device: device.into(),
            broker: Arc::default(),
            open: Some(Arc::new(open)),
            requests: Arc::default(),
        })
    }
    /// Retain an already owned broker; no second vault snapshot is opened.
    /// # Errors
    /// Refuses malformed person or SDK-device labels.
    pub fn from_broker(
        principal: &str,
        device: &str,
        broker: Arc<Mutex<CredentialBroker>>,
    ) -> Result<Self, BrokerError> {
        Self::validate(principal, device)?;
        let owner = Self {
            principal: principal.into(),
            device: device.into(),
            broker: Arc::default(),
            open: None,
            requests: Arc::default(),
        };
        owner.broker.set(broker).map_err(|_| BrokerError::Denied)?;
        Ok(owner)
    }
    fn validate(principal: &str, device: &str) -> Result<(), BrokerError> {
        if [principal, device].iter().any(|value| {
            value.is_empty()
                || value.len() > 512
                || value.chars().any(|c| c.is_control() || c.is_whitespace())
        }) {
            return Err(BrokerError::Denied);
        }
        Ok(())
    }
    pub fn principal(&self) -> &str {
        &self.principal
    }
    pub fn device(&self) -> &str {
        &self.device
    }
    /// Match an already initialized native broker without opening custody.
    pub fn owns_broker(&self, broker: &Arc<Mutex<CredentialBroker>>) -> bool {
        self.broker
            .get()
            .is_some_and(|current| Arc::ptr_eq(current, broker))
    }
    /// Host-only access to the original current permission broker.
    /// # Errors
    /// Refuses an owner without a native initializer or existing broker.
    pub fn broker(&self) -> Result<Arc<Mutex<CredentialBroker>>, BrokerError> {
        if let Some(broker) = self.broker.get() {
            return Ok(broker.clone());
        }
        let open = self.open.as_ref().ok_or(BrokerError::Denied)?;
        Ok(self
            .broker
            .get_or_init(|| Arc::new(Mutex::new(CredentialBroker::new(open()))))
            .clone())
    }
    /// Password-manager reads through the same owner; nothing is serialized.
    /// # Errors
    /// Refuses unavailable or poisoned custody.
    pub fn read<T>(&self, read: impl FnOnce(&SecretStore) -> T) -> Result<T, BrokerError> {
        let broker = self.broker()?;
        let current = broker.lock().map_err(|_| BrokerError::Denied)?;
        Ok(read(&current.store))
    }
    /// Password-manager edits through the same owner. Apps are not lent this door.
    /// # Errors
    /// Refuses unavailable or poisoned custody.
    pub fn edit<T>(&self, edit: impl FnOnce(&mut SecretStore) -> T) -> Result<T, BrokerError> {
        let broker = self.broker()?;
        let mut current = broker.lock().map_err(|_| BrokerError::Denied)?;
        let result = edit(&mut current.store);
        let CredentialBroker {
            store,
            session_grants,
            ..
        } = &mut *current;
        session_grants.retain(|key, _| store.contains(&key.item_id));
        current.revision = current.revision.saturating_add(1);
        drop(current);
        Ok(result)
    }
    /// Nonblocking UI invalidation; neither initializes storage nor waits for I/O.
    pub fn revision(&self) -> Option<u64> {
        self.broker
            .get()?
            .try_lock()
            .ok()
            .map(|broker| broker.revision.saturating_add(self.requests.revision()))
    }
    /// Current grants include the original broker's session permissions.
    /// # Errors
    /// Refuses unavailable or poisoned custody.
    pub fn grants(&self) -> Result<Vec<CredentialGrant>, BrokerError> {
        let broker = self.broker()?;
        let current = broker.lock().map_err(|_| BrokerError::Denied)?;
        Ok(current.current_grants())
    }
    /// Revoke through the real broker, including its current session grant.
    /// # Errors
    /// Refuses unavailable custody or a failed durable revocation.
    pub fn revoke_grant(&self, grant: &CredentialGrant) -> Result<bool, BrokerError> {
        let broker = self.broker()?;
        let mut current = broker.lock().map_err(|_| BrokerError::Denied)?;
        current.revoke_grant(grant)
    }
    /// # Errors
    /// Refuses unavailable custody or a failed durable revocation.
    pub fn revoke_app(&self, app: &str) -> Result<usize, BrokerError> {
        let broker = self.broker()?;
        let mut current = broker.lock().map_err(|_| BrokerError::Denied)?;
        current.revoke_app(app)
    }
}
