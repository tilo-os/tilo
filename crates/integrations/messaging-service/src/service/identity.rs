//! Who an account belongs to, and the key it keeps its state under.
//!
//! An [`Identity`] is an account owner admitted under the person's current
//! device identity while their session is usable. A [`Credential`] binds one
//! account's profile key, an entry of the person's workspace vault, to
//! exactly that owner.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::session::{Call, Reply, SessionLink};
use tilo_identity::identity_store::DeviceIdentity;
use tilo_messaging::SendError;
use tilo_messaging_protocol::journal::Owner;

/// The person's device identity and whether their session is usable now.
pub(crate) struct Ownership {
    host: DeviceIdentity,
    actor: String,
    device: String,
    workspace: collab_types::WorkspaceId,
    active: AtomicBool,
    generation: AtomicU64,
}

impl Ownership {
    /// The device identity of `actor` as this device holds it, on SDK
    /// device `device`.
    ///
    /// # Errors
    /// The device key or the person's certificate cannot be read.
    pub(crate) fn load(actor: &str, device: &str) -> Result<Self, String> {
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        let key =
            tilo_device_identity::persist::load_device_key(&paths).map_err(|e| e.to_string())?;
        let mut person = [0; 32];
        hex::decode_to_slice(actor, &mut person).map_err(|e| e.to_string())?;
        let cert =
            tilo_device_identity::persist::read_device_cert(&paths.device_cert_for_user(&person))
                .map_err(|e| e.to_string())?;
        Self::with_host(
            DeviceIdentity::held(
                cert,
                key,
                std::sync::Arc::new(tilo_identity::identity_store::files::Files::new(paths)),
            ),
            actor,
            device,
        )
    }

    pub(crate) fn with_host(
        host: DeviceIdentity,
        actor: &str,
        device: &str,
    ) -> Result<Self, String> {
        let mut person = [0; 32];
        hex::decode_to_slice(actor, &mut person).map_err(|e| e.to_string())?;
        let room = collab_types::person_workspace(&person).to_string();
        let workspace = collab_types::WorkspaceId::parse(&room)
            .ok_or_else(|| "a person's own workspace is not a workspace id".to_string())?;
        Ok(Self {
            host,
            actor: actor.into(),
            device: device.into(),
            workspace,
            active: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        })
    }

    pub(crate) fn actor(&self) -> &str {
        &self.actor
    }

    pub(crate) fn device(&self) -> &str {
        &self.device
    }

    /// The compositor reports the session usable or not; a new generation
    /// retires every enrollment admitted under an earlier one.
    pub(crate) fn session(&self, active: bool, generation: u64) {
        self.generation.store(generation, Ordering::Release);
        self.active.store(active, Ordering::Release);
    }

    pub(crate) fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Admit `owner` of this person and SDK device now.
    ///
    /// # Errors
    /// Another person or device, an inactive session or changed ownership.
    pub(crate) fn admit(self: &Arc<Self>, owner: Owner) -> Result<Identity, SendError> {
        if owner.actor != self.actor || owner.device != self.device {
            return Err(SendError::Revoked);
        }
        let identity = Identity {
            owner,
            ownership: self.clone(),
            generation: Some(self.generation()),
        };
        identity.check()?;
        Ok(identity)
    }

    /// A new account of `service` for this person: a fresh id, first link
    /// generation.
    pub(crate) fn new_owner(&self, service: tilo_messaging_protocol::Service) -> Owner {
        Owner {
            actor: self.actor.clone(),
            device: self.device.clone(),
            service,
            account: uuid::Uuid::new_v4().to_string(),
            linking_generation: 1,
        }
    }
}

/// An account owner admitted under the person's current ownership.
#[derive(Clone)]
pub(crate) struct Identity {
    pub(crate) owner: Owner,
    ownership: Arc<Ownership>,
    /// The session generation an enrollment was admitted under; `None` for a
    /// running account, which is usable again once the session is.
    generation: Option<u64>,
}

impl Identity {
    /// # Errors
    /// The session is not usable, the enrollment's session ended, or the
    /// device's certificate no longer names this person and device.
    pub(crate) fn check(&self) -> Result<(), SendError> {
        if !self.ownership.active()
            || self
                .generation
                .is_some_and(|generation| generation != self.ownership.generation())
        {
            return Err(SendError::Revoked);
        }
        let (certificate, _) = self
            .ownership
            .host
            .current_material(self.ownership.workspace)
            .map_err(|_| SendError::Revoked)?;
        if self.owner.actor != self.ownership.actor
            || self.owner.device != self.ownership.device
            || hex::encode(certificate.user_public_key) != self.owner.actor
        {
            return Err(SendError::Revoked);
        }
        if certificate.capabilities.foreign {
            return Err(SendError::NotOwnDevice);
        }
        Ok(())
    }

    /// The same owner as a running account: it outlives the enrollment's
    /// session and is checked against whatever session is current.
    pub(crate) fn running(&self) -> Self {
        Self {
            generation: None,
            ..self.clone()
        }
    }
}

pub(crate) fn service_name(service: tilo_messaging_protocol::Service) -> &'static str {
    match service {
        tilo_messaging_protocol::Service::Signal => "signal",
        tilo_messaging_protocol::Service::Whatsapp => "whatsapp",
        tilo_messaging_protocol::Service::Sms => "sms",
        tilo_messaging_protocol::Service::Tilo => "tilo",
    }
}

/// One account's profile key, bound to exactly that account's owner. It is an
/// entry of the person's workspace vault, which the compositor keeps; the
/// running account holds it in memory, wiped when the account goes.
#[derive(Clone)]
pub(crate) struct Credential {
    key: secret_vault::VaultKey,
    /// The vault entry that keeps the key.
    pub(crate) item: String,
    pub(crate) owner: Owner,
}

impl Credential {
    /// A fresh key for `identity`'s new account, kept in the person's
    /// workspace vault for `origin` under `title`. Blocks.
    ///
    /// # Errors
    /// Retired admission, or the compositor did not keep the key.
    pub(crate) fn keep(
        session: &SessionLink,
        identity: &Identity,
        origin: &str,
        title: String,
    ) -> Result<Self, SendError> {
        identity.check()?;
        let key = secret_vault::VaultKey::generate().map_err(|_| SendError::Unavailable)?;
        let call = Call::KeepKey {
            title,
            origin: origin.into(),
            key: zeroize::Zeroizing::new(hex::encode(key.expose_secret())),
        };
        match session.call_blocking(call, KEPT) {
            Some(Reply::Kept { item }) => Self::bound(identity, key, item),
            Some(Reply::Refused) => Err(SendError::Revoked),
            _ => Err(SendError::Unavailable),
        }
    }

    /// The key the vault entry `item` keeps, for `identity`'s published
    /// account. Blocks.
    ///
    /// # Errors
    /// Retired admission, or the vault does not hand out the key.
    pub(crate) fn kept(
        session: &SessionLink,
        identity: &Identity,
        item: String,
    ) -> Result<Self, SendError> {
        identity.check()?;
        let Some(Reply::Key { key }) =
            session.call_blocking(Call::Key { item: item.clone() }, KEPT)
        else {
            return Err(SendError::Revoked);
        };
        let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(key.as_bytes(), bytes.as_mut()).map_err(|_| SendError::Revoked)?;
        let key =
            secret_vault::VaultKey::from_slice(bytes.as_ref()).map_err(|_| SendError::Revoked)?;
        Self::bound(identity, key, item)
    }

    fn bound(
        identity: &Identity,
        key: secret_vault::VaultKey,
        item: String,
    ) -> Result<Self, SendError> {
        let owner = &identity.owner;
        if owner.linking_generation == 0 || owner.account.is_empty() || item.is_empty() {
            return Err(SendError::Revoked);
        }
        identity.check()?;
        Ok(Self {
            key,
            item,
            owner: owner.clone(),
        })
    }

    /// The account's profile key.
    pub(crate) fn profile_key(&self) -> secret_vault::VaultKey {
        self.key.clone()
    }
}

/// How long the compositor has to keep a key or hand one out.
const KEPT: std::time::Duration = std::time::Duration::from_secs(10);
