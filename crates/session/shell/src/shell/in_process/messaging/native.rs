//! Direct Tilo messages retain the original protected personal account gate.
use super::{
    Arc, Authorization, NativeMessagingCredential, NativeMessagingIdentity, Owner, Route,
    SendError, SignalAuthority,
};
use crate::network::private_messages::{self, Text, Transport};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use tilo_messaging_protocol::{
    native_broker::{Assembly, NativeBroker},
    supervisor::AccountSupervisor,
};

pub(super) struct Startup {
    pub identity: NativeMessagingIdentity,
    pub credential: NativeMessagingCredential,
    pub authority: Arc<dyn SignalAuthority>,
}
struct Gate {
    startup: Startup,
    peers: crate::network::PeerIdentityStore,
    scope: String,
    stopped: AtomicBool,
}
impl Gate {
    fn check(&self) -> Result<(), SendError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(SendError::Revoked);
        }
        self.startup.identity.check()?;
        if self.startup.credential.owner != self.startup.identity.owner {
            return Err(SendError::Revoked);
        }
        self.startup.credential.check()
    }
    fn checked(&self, authorization: Authorization) -> Result<Authorization, SendError> {
        self.check()?;
        if authorization.binding.owner != self.startup.identity.owner {
            return Err(SendError::Revoked);
        }
        Ok(authorization)
    }
    fn check_peer(&self, material: &private_messages::Material) -> Result<(), SendError> {
        self.check()?;
        let certificate = &material.cert;
        let mut revocations = tilo_user_identity::RevocationList::new();
        for entry in &material.revocations {
            if entry.user_public_key != certificate.user_public_key {
                return Err(SendError::Revoked);
            }
            revocations
                .insert(entry.clone())
                .map_err(|_| SendError::Revoked)?;
        }
        self.peers
            .set_revocations(certificate.user_public_key, &revocations);
        if self.peers.is_revoked(
            &certificate.user_public_key,
            &certificate.device_public_key,
            tilo_clock::unix_now_ns(),
        ) {
            return Err(SendError::Revoked);
        }
        let address = address(certificate);
        let authorization = self.incoming(&address)?;
        if authorization.route.recipient != address {
            return Err(SendError::Revoked);
        }
        self.authorize(&authorization.route)?;
        self.check()
    }
}
impl SignalAuthority for Gate {
    fn check_account(&self) -> Result<(), SendError> {
        self.check()?;
        self.startup.authority.check_account()?;
        self.check()
    }
    fn check_account_scope(&self, scope: &str) -> Result<(), SendError> {
        if scope != self.scope {
            return Err(SendError::Revoked);
        }
        self.check_account()
    }
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        self.check()?;
        self.checked(self.startup.authority.authorize(route)?)
    }
    fn incoming(&self, recipient: &str) -> Result<Authorization, SendError> {
        self.check()?;
        self.checked(self.startup.authority.incoming(recipient)?)
    }
}
impl tilo_messaging_protocol::supervisor::AccountAuthority for Gate {
    fn check(&self) -> Result<(), SendError> {
        self.check_account()
    }
}
struct Outbound {
    gate: Arc<Gate>,
    transport: Transport,
}
impl tilo_messaging_protocol::native_broker::Transport for Outbound {
    fn send<'a>(
        &'a self,
        request: &'a tilo_messaging::SendRequest,
        timestamp_ms: u64,
    ) -> tilo_messaging_protocol::native_broker::SendText<'a> {
        Box::pin(async move {
            self.gate.authorize(&request.route)?;
            let (root, device) = parse_address(&request.route.recipient)?;
            self.transport
                .send(
                    iroh::EndpointId::from_bytes(&device).map_err(|_| SendError::Revoked)?,
                    root,
                    Text {
                        id: uuid::Uuid::parse_str(&request.operation)
                            .map_err(|_| SendError::InvalidRequest)?,
                        timestamp_ms,
                        body: request.text.to_string(),
                    },
                )
                .await?;
            self.gate
                .authorize(&request.route)
                .map_err(|_| SendError::Uncertain)?;
            Ok(())
        })
    }
}
pub(super) struct Account {
    gate: Arc<Gate>,
    transport: Transport,
    supervisor: Arc<AccountSupervisor>,
    broker: Mutex<Option<NativeBroker>>,
}
impl super::NativeAccountCustody for Account {
    fn owner(&self) -> &Owner {
        &self.gate.startup.identity.owner
    }
}
impl private_messages::Account for Account {
    fn certificate(&self) -> Result<private_messages::Material, SendError> {
        self.gate.check_account()?;
        self.gate
            .startup
            .identity
            .host
            .current_native_material(self.gate.startup.identity.workspace)
            .map(|(cert, revocations)| private_messages::Material { cert, revocations })
            .map_err(|_| SendError::Revoked)
    }
    fn check_peer(&self, material: &private_messages::Material) -> Result<(), SendError> {
        self.gate.check_peer(material)
    }
    fn receive<'a>(
        &'a self,
        material: &'a private_messages::Material,
        text: &'a Text,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SendError>> + Send + 'a>>
    {
        Box::pin(async move {
            let gate = self.gate.clone();
            let checked = material.clone();
            tokio::task::spawn_blocking(move || gate.check_peer(&checked))
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
                    text.id.to_string(),
                    text.body.clone(),
                    text.timestamp_ms,
                )
                .await?;
            let gate = self.gate.clone();
            let checked = material.clone();
            tokio::task::spawn_blocking(move || gate.check_peer(&checked))
                .await
                .map_err(|_| SendError::Unavailable)?
        })
    }
}
impl Account {
    pub(super) fn activate(self: &Arc<Self>, root: &std::path::Path) -> Result<(), SendError> {
        self.gate.check_account()?;
        let owner = &self.gate.startup.identity.owner;
        let history = tilo_messaging_protocol::signal_broker::History::open(
            &root.join("history"),
            owner,
            self.gate.startup.credential.profile_key()?,
        )?;
        self.gate.check_account()?;
        let journal = tilo_messaging_protocol::journal::EncryptedJournal::open(
            &root.join("journal"),
            owner,
            self.gate.startup.credential.profile_key()?,
        )
        .map_err(|_| SendError::Unavailable)?;
        self.gate.check_account()?;
        let broker = NativeBroker::new(Assembly {
            scope: self.gate.scope.clone(),
            authority: self.gate.clone(),
            transport: Arc::new(Outbound {
                gate: self.gate.clone(),
                transport: self.transport.clone(),
            }),
            history,
            journal,
        })?;
        let mut slot = self.broker.lock().map_err(|_| SendError::Unavailable)?;
        if slot.is_some() {
            return Err(SendError::AlreadyReserved);
        }
        *slot = Some(broker.clone());
        drop(slot);
        self.transport
            .bind(&(self.clone() as Arc<dyn private_messages::Account>))?;
        self.gate.check_account()?;
        self.supervisor.install_native(&broker)?;
        self.gate.check_account()
    }
    pub(super) fn state(&self) -> tilo_messaging::AccountState {
        use tilo_messaging::MessagingHost as _;
        self.supervisor
            .account_state(&tilo_messaging::ServiceAccount::new(
                "tilo",
                &self.gate.startup.identity.owner.account,
            ))
    }
    pub(super) fn retire(&self) {
        self.gate.stopped.store(true, Ordering::Release);
        let _ = self
            .supervisor
            .unavailable(tilo_messaging::AccountState::Locked);
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
impl super::super::InProcessApps {
    pub(super) fn prepare_native_tilo_account(
        &mut self,
        mut startup: Startup,
    ) -> Result<Arc<Account>, ui::launch::LaunchError> {
        use ui::launch::LaunchError;
        self.bind_native_messaging_identity(&mut startup.identity)?;
        let owner = &startup.identity.owner;
        if owner.principal != self.author.0
            || owner.device != self.device_id
            || owner.service != tilo_messaging_protocol::Service::Tilo
            || startup.credential.owner != *owner
        {
            return Err(LaunchError::BindingMismatch);
        }
        startup
            .identity
            .check()
            .map_err(|_| LaunchError::BindingMismatch)?;
        startup
            .credential
            .check()
            .map_err(|_| LaunchError::BindingMismatch)?;
        let transport = self
            .native_messaging_transport
            .clone()
            .ok_or(LaunchError::BindingMismatch)?;
        let peers = self
            .native_messaging_peers
            .clone()
            .ok_or(LaunchError::BindingMismatch)?;
        let gate = Arc::new(Gate {
            scope: format!("tilo:{}", owner.account),
            startup,
            peers,
            stopped: AtomicBool::new(false),
        });
        let supervisor =
            AccountSupervisor::new_with_authority(&gate.startup.identity.owner, Some(gate.clone()))
                .map_err(|_| LaunchError::BindingMismatch)?;
        self.install_messaging_supervisor(&supervisor)?;
        let account = Arc::new(Account {
            gate,
            transport,
            supervisor,
            broker: Mutex::new(None),
        });
        self.messaging_native_accounts.push(account.clone());
        Ok(account)
    }
}
