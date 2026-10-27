//! Original native enrollment authority and canonical Settings publication.
//! These handles remain host-private; Messages gains no account editor right.
use super::{AccountAuthority, Arc, LaunchError, NativeMessagingIdentity, Owner, SendError};
use accounts::person::PersonAccounts;
use tilo_messaging_protocol::signal_linking::{
    SignalEnrollment, SignalPairing, VerifiedSignalEnrollment,
};

struct ProvisioningAuthority(NativeMessagingIdentity);
impl AccountAuthority for ProvisioningAuthority {
    fn check(&self) -> Result<(), SendError> {
        self.0.check()
    }
}

/// One native preparation, retaining the original signed personal execution.
pub struct NativeSignalEnrollment {
    identity: NativeMessagingIdentity,
    linker: SignalEnrollment,
}
impl NativeSignalEnrollment {
    pub(super) fn from_original(
        identity: NativeMessagingIdentity,
        stream: std::os::unix::net::UnixStream,
    ) -> Result<Self, SendError> {
        identity.check()?;
        let linker = SignalEnrollment::new(
            identity.owner.clone(),
            Arc::new(ProvisioningAuthority(identity.clone())),
            stream,
        )?;
        Ok(Self { identity, linker })
    }
    pub fn owner(&self) -> &Owner {
        self.linker.owner()
    }
    pub fn state(&self) -> tilo_messaging::AccountState {
        self.linker.state()
    }
    pub fn wake_after(&self) -> Option<std::time::Duration> {
        self.linker.wake_after()
    }
    /// # Errors
    /// Refuses retired original admission, replay or invalid private data.
    pub async fn begin(&self) -> Result<SignalPairing, SendError> {
        self.linker.begin().await
    }
    /// # Errors
    /// Refuses expired attempts, another identity or retired original admission.
    pub async fn finish(
        &self,
        attempt: String,
    ) -> Result<NativeVerifiedSignalEnrollment, SendError> {
        let verified = self.linker.finish(attempt).await?;
        if self.identity.check().is_err() {
            self.linker.cancel();
            return Err(SendError::Revoked);
        }
        Ok(NativeVerifiedSignalEnrollment {
            identity: self.identity.clone(),
            verified,
        })
    }
    pub fn cancel(&self) {
        self.linker.cancel();
    }
}

/// No public constructor, serialization or cloning: a protocol receipt alone
/// cannot substitute for this host's original signed personal execution.
pub struct NativeVerifiedSignalEnrollment {
    pub(super) identity: NativeMessagingIdentity,
    verified: VerifiedSignalEnrollment,
}
impl NativeVerifiedSignalEnrollment {
    pub fn account(&self) -> &tilo_messaging_protocol::signal_setup::SignalAccount {
        self.verified.account()
    }
    pub fn owner(&self) -> &Owner {
        self.verified.owner()
    }
}

impl super::super::InProcessApps {
    pub(super) fn publish_native_whatsapp_account(
        &self,
        identity: &NativeMessagingIdentity,
        name: String,
        jid: String,
    ) -> Result<(), LaunchError> {
        let owner = &identity.owner;
        if owner.principal != self.author.0
            || owner.device != self.device_id
            || owner.service != tilo_messaging_protocol::Service::Whatsapp
        {
            return Err(LaunchError::BindingMismatch);
        }
        self.change_accounts(
            PersonAccounts::PUBLISH_WHATSAPP_ACCOUNT.with((
                owner.account.clone(),
                name,
                jid,
                owner.linking_generation,
            )),
            identity,
        )
    }
    /// Change the person's accounts by `invoke` if `identity`'s admission
    /// still lets it, checked as the change is applied.
    fn change_accounts(
        &self,
        invoke: tilo_resource::Invoke<PersonAccounts>,
        identity: &NativeMessagingIdentity,
    ) -> Result<(), LaunchError> {
        let admitted = identity.clone();
        self.person_records
            .change_accounts(invoke, move || {
                admitted
                    .check()
                    .map_err(|_| "original native admission retired".to_owned())
            })
            .map_err(LaunchError::Operation)
    }
    /// Prepare private key custody only for the exact unpublished enrollment
    /// that passed provider self-identity verification. The protected Passwords
    /// decision remains separate; this door creates no grant or ready adapter.
    /// # Errors
    /// Refuses foreign/retired evidence, weaker custody or an occupied scope.
    pub fn prepare_native_signal_profile_key(
        &self,
        candidate: &NativeVerifiedSignalEnrollment,
        origin: &str,
    ) -> Result<accounts::SecretId, LaunchError> {
        if candidate.owner() != &candidate.identity.owner {
            return Err(LaunchError::BindingMismatch);
        }
        self.prepare_native_messaging_profile_key(&candidate.identity, origin)
    }
    pub(super) fn prepare_native_messaging_profile_key(
        &self,
        identity: &NativeMessagingIdentity,
        origin: &str,
    ) -> Result<accounts::SecretId, LaunchError> {
        let owner = &identity.owner;
        let vault = self
            .credential_owner
            .as_ref()
            .ok_or(LaunchError::BindingMismatch)?;
        let socket = self
            .native_broker_socket
            .as_ref()
            .ok_or(LaunchError::BindingMismatch)?;
        if owner.principal != self.author.0
            || owner.device != self.device_id
            || owner.principal != vault.principal()
            || owner.device != vault.device()
            || !identity
                .native_session
                .as_ref()
                .is_some_and(|session| session.matches_socket(socket))
        {
            return Err(LaunchError::BindingMismatch);
        }
        identity.check().map_err(|_| LaunchError::BindingMismatch)?;
        let id = vault
            .prepare_profile_key(&super::credential_scope(owner), origin)
            .map_err(|_| LaunchError::BindingMismatch)?;
        if identity.check().is_err() {
            // This call created a new item, never an existing key or grant.
            vault
                .edit(|store| store.remove(&id))
                .map_err(|_| LaunchError::BindingMismatch)?
                .map_err(|_| LaunchError::BindingMismatch)?;
            return Err(LaunchError::BindingMismatch);
        }
        Ok(id)
    }
    /// Prepare an unpublished attempt on a separate isolated host-owned stream.
    /// The native setup door confirms linking before `begin`; preparation does
    /// no provider/credential I/O and chooses no account by a person's name.
    /// # Errors
    /// Refuses absent/retired original admission or unusable owned handles.
    pub fn prepare_native_signal_enrollment(
        &self,
        stream: std::os::unix::net::UnixStream,
    ) -> Result<NativeSignalEnrollment, LaunchError> {
        let owner = Owner {
            principal: self.author.0.clone(),
            device: self.device_id.clone(),
            service: tilo_messaging_protocol::Service::Signal,
            account: uuid::Uuid::new_v4().to_string(),
            linking_generation: 1,
        };
        let identity = self.native_messaging_identity(owner)?;
        NativeSignalEnrollment::from_original(identity, stream)
            .map_err(|_| LaunchError::BindingMismatch)
    }

    /// Publish the original native account `identity` owns through
    /// `publish`, refusing an identity of any service but `service`.
    pub(super) fn publish_native_account<
        P: serde::Serialize + From<(String, String, String, u64)>,
    >(
        &self,
        identity: &NativeMessagingIdentity,
        service: tilo_messaging_protocol::Service,
        publish: tilo_resource::ActionRef<PersonAccounts, P>,
        name: String,
        scope: &str,
    ) -> Result<(), LaunchError> {
        if identity.owner.service != service
            || identity.owner.principal != self.author.0
            || identity.owner.device != self.device_id
        {
            return Err(LaunchError::BindingMismatch);
        }
        self.change_accounts(
            publish.with((
                identity.owner.account.clone(),
                name,
                scope.to_owned(),
                identity.owner.linking_generation,
            )),
            identity,
        )
    }
    /// Publish only after the confirmed self-identity review. This invokes the
    /// canonical Settings action through its original person-record owner;
    /// it creates no credential, permission, messaging loan or ready adapter.
    /// # Errors
    /// Refuses foreign/retired native evidence, occupied identities or storage failure.
    pub fn publish_native_signal_enrollment(
        &mut self,
        mut candidate: NativeVerifiedSignalEnrollment,
        display_name: String,
    ) -> Result<tilo_messaging::LinkedAccount, LaunchError> {
        if candidate.owner() != &candidate.identity.owner
            || candidate.owner().principal != self.author.0
            || candidate.owner().device != self.device_id
        {
            return Err(LaunchError::BindingMismatch);
        }
        self.bind_native_messaging_identity(&mut candidate.identity)?;
        let (owner, account) = candidate
            .verified
            .accept()
            .map_err(|_| LaunchError::BindingMismatch)?;
        let invoke = PersonAccounts::PUBLISH_SIGNAL_ACCOUNT.with((
            owner.account.clone(),
            display_name,
            account.aci.clone(),
            owner.linking_generation,
        ));
        self.change_accounts(invoke, &candidate.identity)?;
        Ok(tilo_messaging::LinkedAccount {
            account: tilo_messaging::ServiceAccount::new("signal", owner.account),
            generation: owner.linking_generation,
            scope: account.scope(),
        })
    }
}
