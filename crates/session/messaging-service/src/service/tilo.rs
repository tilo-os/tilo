//! Direct Tilo messages, carried by the device's own endpoint in the
//! compositor and kept here under the account's key and gate.
//!
//! The compositor checks a peer's device against the revocations it knows
//! and hands this account the rest: whether the person's contacts route the
//! peer to this account, and the text to keep. Sends go out the same way.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_messaging::{SendError, SendRequest};
use tilo_messaging_protocol::native_broker::{Assembly, NativeBroker, SendText, Transport};
use tilo_messaging_protocol::signal_broker::SignalAuthority as _;
use tilo_messaging_protocol::supervisor::AccountSupervisor;

use super::gate::{Gate, Running};
use crate::door::{Call, DoorLink, Material, Reply, Text};

/// How long a send may take on the endpoint; a timeout is uncertain and
/// never resent.
const SEND: Duration = Duration::from_secs(25);

pub(crate) struct Account {
    gate: Arc<Gate>,
    supervisor: Arc<AccountSupervisor>,
    broker: Mutex<Option<NativeBroker>>,
    door: DoorLink,
}

impl Running for Account {
    fn gate(&self) -> &Arc<Gate> {
        &self.gate
    }
    fn supervisor(&self) -> &Arc<AccountSupervisor> {
        &self.supervisor
    }
}

struct Outbound {
    gate: Arc<Gate>,
    door: DoorLink,
}

impl Transport for Outbound {
    fn send<'a>(&'a self, request: &'a SendRequest, timestamp_ms: u64) -> SendText<'a> {
        Box::pin(async move {
            self.gate.authorize(&request.route)?;
            let (root, peer) = parse_address(&request.route.recipient)?;
            uuid::Uuid::parse_str(&request.operation).map_err(|_| SendError::InvalidRequest)?;
            let answer = self.door.call(Call::PrivateSend {
                peer,
                root,
                text: Text {
                    id: request.operation.clone(),
                    timestamp_ms,
                    body: zeroize::Zeroizing::new(request.text.to_string()),
                },
            });
            match tokio::time::timeout(SEND, answer).await {
                Ok(Ok(Reply::Done)) => {}
                Ok(Ok(Reply::Refused)) => return Err(SendError::Revoked),
                _ => return Err(SendError::Uncertain),
            }
            self.gate
                .authorize(&request.route)
                .map_err(|_| SendError::Uncertain)?;
            Ok(())
        })
    }
}

impl Account {
    pub(crate) fn new(gate: Arc<Gate>, supervisor: Arc<AccountSupervisor>, door: DoorLink) -> Self {
        Self {
            gate,
            supervisor,
            broker: Mutex::new(None),
            door,
        }
    }

    /// Open the account's history and journal and have the compositor's
    /// endpoint deliver this device's private texts to it. Blocks.
    pub(crate) fn activate(&self, root: &std::path::Path) -> Result<(), SendError> {
        self.gate.check_account()?;
        let owner = &self.gate.identity.owner;
        let history = tilo_messaging_protocol::signal_broker::History::open(
            &root.join("history"),
            owner,
            self.gate.credential.profile_key()?,
        )?;
        self.gate.check_account()?;
        let journal = tilo_messaging_protocol::journal::EncryptedJournal::open(
            &root.join("journal"),
            owner,
            self.gate.credential.profile_key()?,
        )
        .map_err(|_| SendError::Unavailable)?;
        self.gate.check_account()?;
        let broker = NativeBroker::new(Assembly {
            scope: format!("tilo:{}", owner.account),
            authority: self.gate.clone(),
            transport: Arc::new(Outbound {
                gate: self.gate.clone(),
                door: self.door.clone(),
            }),
            history,
            journal,
        })?;
        {
            let mut slot = self.broker.lock().map_err(|_| SendError::Unavailable)?;
            if slot.is_some() {
                return Err(SendError::AlreadyReserved);
            }
            *slot = Some(broker.clone());
        }
        match self
            .door
            .call_blocking(Call::PrivateBind, Duration::from_secs(10))
        {
            Some(Reply::Done) => {}
            _ => return Err(SendError::Unavailable),
        }
        self.gate.check_account()?;
        self.supervisor.install_native(&broker)?;
        self.gate.check_account()
    }

    /// This device's certificate for the account, as peers see it.
    pub(crate) fn certificate(&self) -> Result<Material, SendError> {
        self.gate.check_account()?;
        let custody = self.gate.identity.custody();
        custody
            .host()
            .current_native_material(custody.workspace())
            .map(|(cert, revocations)| Material { cert, revocations })
            .map_err(|_| SendError::Revoked)
    }

    /// Whether the person's contacts route `material`'s device to this
    /// account, both ways.
    pub(crate) fn check_peer(&self, material: &Material) -> Result<(), SendError> {
        self.gate.check()?;
        if material
            .revocations
            .iter()
            .any(|entry| entry.user_public_key != material.cert.user_public_key)
        {
            return Err(SendError::Revoked);
        }
        let address = address(&material.cert);
        let authorization = self.gate.incoming(&address)?;
        if authorization.route.recipient != address {
            return Err(SendError::Revoked);
        }
        self.gate.authorize(&authorization.route)?;
        self.gate.check()
    }

    /// Keep a text from `material`'s device; answered once it is durable.
    pub(crate) async fn receive(
        self: Arc<Self>,
        material: Material,
        text: Text,
    ) -> Result<(), SendError> {
        let checking = self.clone();
        let checked = material.clone();
        tokio::task::spawn_blocking(move || checking.check_peer(&checked))
            .await
            .map_err(|_| SendError::Unavailable)??;
        let broker = self
            .broker
            .lock()
            .map_err(|_| SendError::Unavailable)?
            .clone()
            .ok_or(SendError::Unavailable)?;
        broker
            .receive(
                address(&material.cert),
                text.id.clone(),
                text.body.to_string(),
                text.timestamp_ms,
            )
            .await?;
        let checking = self.clone();
        tokio::task::spawn_blocking(move || checking.check_peer(&material))
            .await
            .map_err(|_| SendError::Unavailable)?
    }
}

fn address(certificate: &tilo_user_identity::DeviceCert) -> String {
    format!(
        "{}@{}",
        hex::encode(certificate.user_public_key),
        hex::encode(certificate.device_public_key)
    )
}

fn parse_address(value: &str) -> Result<([u8; 32], [u8; 32]), SendError> {
    let (root, device) = value.split_once('@').ok_or(SendError::Revoked)?;
    let mut person = [0; 32];
    let mut peer = [0; 32];
    hex::decode_to_slice(root, &mut person).map_err(|_| SendError::Revoked)?;
    hex::decode_to_slice(device, &mut peer).map_err(|_| SendError::Revoked)?;
    if root != hex::encode(person) || device != hex::encode(peer) {
        return Err(SendError::Revoked);
    }
    Ok((person, peer))
}

/// Tilo's steps in the one enrollment flow: no link, one account per person.
pub(crate) struct Tilo;

impl super::flow::Provider for Tilo {
    type Held = ();
    type Account = Account;
    const SERVICE: tilo_messaging_protocol::Service = tilo_messaging_protocol::Service::Tilo;
    // The key's provenance label under the canonical origin policy; no
    // request is made to it.
    const ORIGIN: &'static str = "https://tilo.so";
    const ORDER: super::flow::Order = super::flow::Order::Grant;

    fn purpose(_name: &str) -> String {
        "Private Tilo messaging using this personal identity".into()
    }

    fn admits(&self, service: &super::Service) -> Result<(), SendError> {
        if service.runs_service("tilo") {
            return Err(SendError::Unavailable);
        }
        if service
            .person_accounts()?
            .all()
            .iter()
            .any(|row| row.enabled && row.type_id.as_str() == "tilo")
        {
            return Err(SendError::AlreadyReserved);
        }
        Ok(())
    }

    fn number(identity: &super::identity::Identity) -> Option<String> {
        Some(identity.owner.principal.clone())
    }

    fn approved(
        &self,
        service: &Arc<super::Service>,
        identity: &super::identity::Identity,
        credential: super::identity::Credential,
        name: &str,
        _held: Option<()>,
    ) -> Result<(Arc<Account>, String, Option<()>), SendError> {
        let scope = format!("tilo:{}", identity.owner.account);
        service.publish(
            identity,
            crate::door::Publication {
                account: identity.owner.account.clone(),
                name: name.into(),
                generation: identity.owner.linking_generation,
                kind: crate::door::PublicationKind::Tilo {
                    scope: scope.clone(),
                },
            },
        )?;
        let account = start(service, identity, credential, &scope)?;
        Ok((account, scope, None))
    }

    fn restorable(rows: usize) -> bool {
        rows == 1
    }

    fn resume(
        &self,
        service: &Arc<super::Service>,
        identity: &super::identity::Identity,
        credential: super::identity::Credential,
        scope: String,
        _row: &accounts::Account,
    ) -> Result<Arc<Account>, SendError> {
        start(service, identity, credential, &scope)
    }
}

fn start(
    service: &Arc<super::Service>,
    identity: &super::identity::Identity,
    credential: super::identity::Credential,
    scope: &str,
) -> Result<Arc<Account>, SendError> {
    let door = service.door.clone();
    let account = service.account(identity, credential, scope, |gate, supervisor| {
        Account::new(gate, supervisor, door)
    })?;
    service.bind_private(&account);
    account.activate(&service.private_root(&identity.owner)?)?;
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_names_both_keys_in_lowercase_hex_and_nothing_else() {
        let root = "ab".repeat(32);
        let device = "cd".repeat(32);
        assert_eq!(
            parse_address(&format!("{root}@{device}")),
            Ok(([0xab; 32], [0xcd; 32]))
        );
        assert!(parse_address(&format!("{}@{device}", root.to_uppercase())).is_err());
        assert!(parse_address(&root).is_err());
    }
}
