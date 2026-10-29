//! Who an account belongs to, and what it was granted.
//!
//! An [`Identity`] is an account owner admitted under the person's current
//! device custody while their session is usable. A [`Credential`] binds one
//! vault item Messages was granted to exactly that owner. A [`Permission`]
//! is the request Passwords answers before a new account gets its key.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use accounts::{AccessDecision, AccessRequest, CredentialBroker, CredentialOwner, CredentialScope};
use tilo_identity_host::custody::HostIdentity;
use tilo_messaging::SendError;
use tilo_messaging_protocol::journal::Owner;

/// The person's device custody and whether their session is usable now.
pub(crate) struct Custody {
    host: HostIdentity,
    principal: String,
    device: String,
    workspace: u64,
    active: AtomicBool,
    generation: AtomicU64,
}

impl Custody {
    /// The device custody of `principal` as this device holds it, on SDK
    /// device `device`.
    ///
    /// # Errors
    /// The device key or the person's certificate cannot be read.
    pub(crate) fn load(principal: &str, device: &str) -> Result<Self, String> {
        let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
        let key =
            tilo_user_identity::persist::load_device_key(&paths).map_err(|e| e.to_string())?;
        let mut person = [0; 32];
        hex::decode_to_slice(principal, &mut person).map_err(|e| e.to_string())?;
        let cert =
            tilo_user_identity::persist::read_device_cert(&paths.device_cert_for_user(&person))
                .map_err(|e| e.to_string())?;
        Self::with_host(
            HostIdentity {
                device_cert: cert,
                device_signing_key: key,
                workspace_device_certs: std::collections::HashMap::new(),
                workspace_revocations: std::collections::HashMap::new(),
                identity_paths: Some(paths),
            },
            principal,
            device,
        )
    }

    pub(crate) fn with_host(
        host: HostIdentity,
        principal: &str,
        device: &str,
    ) -> Result<Self, String> {
        let mut person = [0; 32];
        hex::decode_to_slice(principal, &mut person).map_err(|e| e.to_string())?;
        let room = collab_types::person_room(&person).to_string();
        let workspace = tilo_relay_protocol::room_workspace_id(&room).map_err(|e| e.to_string())?;
        Ok(Self {
            host,
            principal: principal.into(),
            device: device.into(),
            workspace,
            active: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        })
    }

    pub(crate) fn principal(&self) -> &str {
        &self.principal
    }

    pub(crate) fn device(&self) -> &str {
        &self.device
    }

    pub(crate) fn host(&self) -> &HostIdentity {
        &self.host
    }

    pub(crate) fn workspace(&self) -> u64 {
        self.workspace
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
    /// Another person or device, an inactive session or changed custody.
    pub(crate) fn admit(self: &Arc<Self>, owner: Owner) -> Result<Identity, SendError> {
        if owner.principal != self.principal || owner.device != self.device {
            return Err(SendError::Revoked);
        }
        let identity = Identity {
            owner,
            custody: self.clone(),
            generation: Some(self.generation()),
        };
        identity.check()?;
        Ok(identity)
    }

    /// A new account of `service` for this person: a fresh id, first link
    /// generation.
    pub(crate) fn new_owner(&self, service: tilo_messaging_protocol::Service) -> Owner {
        Owner {
            principal: self.principal.clone(),
            device: self.device.clone(),
            service,
            account: uuid::Uuid::new_v4().to_string(),
            linking_generation: 1,
        }
    }
}

/// An account owner admitted under the person's current custody.
#[derive(Clone)]
pub(crate) struct Identity {
    pub(crate) owner: Owner,
    custody: Arc<Custody>,
    /// The session generation an enrollment was admitted under; `None` for a
    /// running account, which is usable again once the session is.
    generation: Option<u64>,
}

impl Identity {
    /// # Errors
    /// The session is not usable, the enrollment's session ended, or the
    /// device's certificate no longer names this person and device.
    pub(crate) fn check(&self) -> Result<(), SendError> {
        if !self.custody.active()
            || self
                .generation
                .is_some_and(|generation| generation != self.custody.generation())
        {
            return Err(SendError::Revoked);
        }
        let (certificate, _) = self
            .custody
            .host
            .current_native_material(self.custody.workspace)
            .map_err(|_| SendError::Revoked)?;
        if self.owner.principal != self.custody.principal
            || self.owner.device != self.custody.device
            || hex::encode(certificate.user_public_key) != self.owner.principal
            || certificate.capabilities.foreign
        {
            return Err(SendError::Revoked);
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

    pub(crate) fn custody(&self) -> &Arc<Custody> {
        &self.custody
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

pub(crate) fn credential_scope(owner: &Owner) -> CredentialScope {
    CredentialScope {
        principal: owner.principal.clone(),
        device: owner.device.clone(),
        service: service_name(owner.service).into(),
        account: owner.account.clone(),
        generation: owner.linking_generation,
    }
}

/// One vault item Messages was granted, bound to one account owner. The
/// broker is the vault's own: revoking the grant in Passwords revokes it here.
#[derive(Clone)]
pub(crate) struct Credential {
    broker: Arc<Mutex<CredentialBroker>>,
    pub(crate) request: AccessRequest,
    pub(crate) owner: Owner,
}

impl Credential {
    /// # Errors
    /// Another app, an unscoped request or an owner without an account.
    pub(crate) fn new(
        vault: &CredentialOwner,
        identity: &Identity,
        request: AccessRequest,
    ) -> Result<Self, SendError> {
        let owner = &identity.owner;
        if request.app_id != crate::MESSAGES_APP
            || request.origin.is_none()
            || owner.linking_generation == 0
            || owner.account.is_empty()
            || vault.principal() != owner.principal
            || vault.device() != owner.device
        {
            return Err(SendError::Revoked);
        }
        identity.check()?;
        Ok(Self {
            broker: vault.broker().map_err(|_| SendError::Unavailable)?,
            request,
            owner: owner.clone(),
        })
    }

    /// # Errors
    /// The grant is gone.
    pub(crate) fn check(&self) -> Result<(), SendError> {
        // The decrypted value clears itself at once; nothing copies it.
        self.broker
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .request_existing_scoped(&self.request, &credential_scope(&self.owner))
            .map(drop)
            .map_err(|_| SendError::Revoked)
    }

    /// The account's profile key.
    ///
    /// # Errors
    /// The grant is gone or the item holds no key.
    pub(crate) fn profile_key(&self) -> Result<secret_vault::VaultKey, SendError> {
        let encoded = self
            .broker
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .request_existing_profile_key(&self.request, &credential_scope(&self.owner))
            .map_err(|_| SendError::Revoked)?;
        let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(encoded.as_bytes(), bytes.as_mut())
            .map_err(|_| SendError::Unavailable)?;
        secret_vault::VaultKey::from_slice(bytes.as_ref()).map_err(|_| SendError::Unavailable)
    }
}

/// The key prepared for a new account and the request Passwords answers to
/// grant Messages that key.
pub(crate) struct Permission {
    pub(crate) request: AccessRequest,
    pub(crate) proposal: String,
}

/// Where the person stands on a [`Permission`].
pub(crate) enum Grant {
    Approved(Credential),
    Pending,
    Refused,
}

impl Permission {
    /// Prepare a fresh key for `identity` and ask the person to let Messages
    /// use it. When the request cannot be made, the key is removed again.
    ///
    /// # Errors
    /// Retired admission, weaker custody, an occupied scope or a full queue.
    pub(crate) fn ask(
        vault: &CredentialOwner,
        identity: &Identity,
        origin: &str,
        purpose: String,
    ) -> Result<Self, SendError> {
        identity.check()?;
        let scope = credential_scope(&identity.owner);
        let item_id = vault
            .prepare_profile_key(&scope, origin)
            .map_err(|_| SendError::Revoked)?;
        let request = AccessRequest {
            app_id: crate::MESSAGES_APP.into(),
            item_id,
            purpose,
            origin: Some(origin.into()),
        };
        let retained = identity.clone();
        let proposal = identity
            .check()
            .map_err(|_| accounts::BrokerError::Denied)
            .and_then(|()| {
                vault.propose_scoped(
                    request.clone(),
                    scope,
                    Arc::new(move || retained.check().is_ok()),
                )
            });
        if let Ok(proposal) = proposal {
            Ok(Self { request, proposal })
        } else {
            // Only the item this call created goes, never another key.
            let _ = vault.edit(|store| store.remove(&request.item_id));
            Err(SendError::Revoked)
        }
    }

    pub(crate) fn grant(&self, vault: &CredentialOwner, identity: &Identity) -> Grant {
        if let Ok(credential) = Credential::new(vault, identity, self.request.clone())
            && credential.check().is_ok()
        {
            return Grant::Approved(credential);
        }
        let pending = vault
            .proposals()
            .is_ok_and(|proposals| proposals.iter().any(|p| p.id == self.proposal));
        if pending {
            Grant::Pending
        } else {
            Grant::Refused
        }
    }

    /// Deny the request and remove the key it prepared.
    pub(crate) fn withdraw(&self, vault: &CredentialOwner) {
        let _ = vault.decide_proposal(&self.proposal, AccessDecision::Deny);
        let _ = vault.edit(|store| store.remove(&self.request.item_id));
    }
}
