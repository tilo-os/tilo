//! SMS through a SIM the person chose, under the account's own key and grant.
//! The service never unlocks a SIM or changes the default modem.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tilo_messaging::{AccountState, SendError};
use tilo_messaging_protocol::external_broker::ExternalBroker;
use tilo_messaging_protocol::sms_runtime::{DbusTransport, SimBinding, SmsRuntime};
use tilo_messaging_protocol::supervisor::AccountSupervisor;

use super::gate::{Gate, Running};
use super::identity::Identity;

/// A SIM the person may choose: the bus it is on, its binding and the label
/// shown for it (operator and the last digits of its ICCID, never a number).
pub(crate) struct Selected {
    pub(crate) connection: zbus::Connection,
    pub(crate) binding: SimBinding,
    pub(crate) label: String,
}

/// The system bus.
///
/// # Errors
/// The bus is not reachable.
pub(crate) async fn connection() -> Result<zbus::Connection, SendError> {
    zbus::Connection::system()
        .await
        .map_err(|_| SendError::Unavailable)
}

/// The SIMs `ModemManager` offers for messaging now.
///
/// # Errors
/// No `ModemManager`, too many objects, or its owner changed while reading.
pub(crate) async fn choices(identity: &Identity) -> Result<Vec<Selected>, SendError> {
    identity.check()?;
    let connection = connection().await?;
    let found = tokio::time::timeout(Duration::from_secs(10), capture(&connection))
        .await
        .map_err(|_| SendError::Unavailable)??;
    identity.check()?;
    Ok(found
        .into_iter()
        .map(|(binding, label)| Selected {
            connection: connection.clone(),
            binding,
            label,
        })
        .collect())
}

type Objects = std::collections::HashMap<
    zbus::zvariant::OwnedObjectPath,
    BTreeMap<String, BTreeMap<String, zbus::zvariant::OwnedValue>>,
>;

pub(crate) async fn capture(
    connection: &zbus::Connection,
) -> Result<Vec<(SimBinding, String)>, SendError> {
    let bus = zbus::fdo::DBusProxy::new(connection)
        .await
        .map_err(|_| SendError::Unavailable)?;
    let name = || {
        "org.freedesktop.ModemManager1"
            .try_into()
            .map_err(|_| SendError::Unavailable)
    };
    let daemon = bus
        .get_name_owner(name()?)
        .await
        .map_err(|_| SendError::Unavailable)?;
    let manager = zbus::Proxy::new(
        connection,
        daemon.clone(),
        "/org/freedesktop/ModemManager1",
        "org.freedesktop.DBus.ObjectManager",
    )
    .await
    .map_err(|_| SendError::Unavailable)?;
    let objects: Objects = manager
        .call("GetManagedObjects", &())
        .await
        .map_err(|_| SendError::Unavailable)?;
    if objects.len() > 128 {
        return Err(SendError::Unavailable);
    }
    let mut choices = Vec::new();
    for (path, interfaces) in &objects {
        let Some(modem) = interfaces.get("org.freedesktop.ModemManager1.Modem") else {
            continue;
        };
        if !interfaces.contains_key("org.freedesktop.ModemManager1.Modem.Messaging") {
            continue;
        }
        let Some(sim_path) = modem
            .get("Sim")
            .and_then(|v| <&zbus::zvariant::ObjectPath<'_>>::try_from(v).ok())
        else {
            continue;
        };
        let Some(sim) = objects
            .get(sim_path)
            .and_then(|i| i.get("org.freedesktop.ModemManager1.Sim"))
        else {
            continue;
        };
        let Some(iccid) = sim
            .get("SimIdentifier")
            .and_then(|v| <&str>::try_from(v).ok())
        else {
            continue;
        };
        let operator = sim
            .get("OperatorName")
            .and_then(|v| <&str>::try_from(v).ok())
            .filter(|v| !v.is_empty() && v.len() <= 160 && !v.chars().any(char::is_control))
            .unwrap_or("");
        let suffix = iccid
            .get(iccid.len().saturating_sub(4)..)
            .ok_or(SendError::Unavailable)?;
        let label = if operator.is_empty() {
            format!("…{suffix}")
        } else {
            format!("{operator} · …{suffix}")
        };
        let binding = SimBinding::new(
            daemon.to_string(),
            path.to_string(),
            sim_path.to_string(),
            iccid.into(),
            1,
        )
        .map_err(|_| SendError::Unavailable)?;
        choices.push((binding, label));
        if choices.len() > 32 {
            return Err(SendError::Unavailable);
        }
    }
    if bus
        .get_name_owner(name()?)
        .await
        .map_err(|_| SendError::Unavailable)?
        != daemon
    {
        return Err(SendError::Revoked);
    }
    Ok(choices)
}

/// A running SMS account.
pub(crate) struct Account {
    gate: Arc<Gate>,
    supervisor: Arc<AccountSupervisor>,
}

impl Running for Account {
    fn gate(&self) -> &Arc<Gate> {
        &self.gate
    }
    fn supervisor(&self) -> &Arc<AccountSupervisor> {
        &self.supervisor
    }
}

impl Account {
    pub(crate) fn new(gate: Arc<Gate>, supervisor: Arc<AccountSupervisor>) -> Self {
        Self { gate, supervisor }
    }

    /// Start on the SIM the person chose, saving its binding under the
    /// account's key for the next start.
    pub(crate) async fn activate(
        &self,
        selected: Selected,
        root: std::path::PathBuf,
        scope: String,
    ) -> Result<(), SendError> {
        let result = self
            .assemble(selected.connection, selected.binding, root, scope, true)
            .await;
        if result.is_err() {
            let state = if self.gate.check().is_ok() {
                AccountState::Failed
            } else {
                AccountState::Locked
            };
            let _ = self.supervisor.unavailable(state);
        }
        result
    }

    async fn assemble(
        &self,
        connection: zbus::Connection,
        binding: SimBinding,
        root: std::path::PathBuf,
        scope: String,
        save: bool,
    ) -> Result<(), SendError> {
        self.gate.check()?;
        if !root.is_absolute() {
            return Err(SendError::InvalidRequest);
        }
        let owner = &self.gate.identity.owner;
        let transport = DbusTransport::new(connection, binding.clone(), self.gate.clone())
            .map_err(|_| SendError::Revoked)?;
        transport.validate().await.map_err(|_| SendError::Revoked)?;
        self.gate.check()?;
        if save {
            tilo_files::native::private::directory(&root).map_err(|_| SendError::Unavailable)?;
            self.gate.check()?;
            binding
                .save(
                    &root.join("sim.enc"),
                    owner,
                    &self.gate.credential.profile_key()?,
                )
                .map_err(|_| SendError::Unavailable)?;
            self.gate.check()?;
        }
        let journal = tilo_messaging_protocol::journal::EncryptedJournal::open(
            &root.join("journal"),
            owner,
            self.gate.credential.profile_key()?,
        )
        .map_err(|_| SendError::Unavailable)?;
        self.gate.check()?;
        let history = tilo_messaging_protocol::signal_broker::History::open(
            &root.join("history"),
            owner,
            self.gate.credential.profile_key()?,
        )?;
        self.gate.check()?;
        let runtime = SmsRuntime::new(owner.clone(), transport, journal)
            .map_err(|_| SendError::Unavailable)?;
        let broker = ExternalBroker::new(Box::new(runtime), history, scope, self.gate.clone())?;
        self.gate.check()?;
        self.supervisor.install_sms_receiving(&broker).await?;
        self.gate.check()
    }

    /// Start again on the SIM saved at enrollment.
    pub(crate) async fn resume(
        &self,
        connection: zbus::Connection,
        root: std::path::PathBuf,
        scope: String,
    ) -> Result<(), SendError> {
        self.gate.check()?;
        let binding = SimBinding::load(
            &root.join("sim.enc"),
            &self.gate.identity.owner,
            &self.gate.credential.profile_key()?,
            &connection,
        )
        .await
        .map_err(|_| SendError::Revoked)?;
        self.gate.check()?;
        self.assemble(connection, binding, root, scope, false).await
    }
}

/// SMS's steps in the one enrollment flow.
pub(crate) struct Sms;

impl super::flow::Provider for Sms {
    type Held = Selected;
    type Account = Account;
    const SERVICE: tilo_messaging_protocol::Service = tilo_messaging_protocol::Service::Sms;
    const ORIGIN: &'static str = "https://modemmanager.org";
    const ORDER: super::flow::Order = super::flow::Order::ChooseThenGrant;

    fn purpose(_name: &str) -> String {
        "Private SMS messaging using the selected SIM".into()
    }

    fn choices(
        &self,
        _service: &Arc<super::Service>,
        identity: &Identity,
    ) -> Result<Vec<(String, Selected)>, SendError> {
        let found = tokio::runtime::Handle::current().block_on(choices(identity))?;
        Ok(found
            .into_iter()
            .map(|selected| (selected.label.clone(), selected))
            .collect())
    }

    fn approved(
        &self,
        service: &Arc<super::Service>,
        identity: &Identity,
        credential: super::identity::Credential,
        name: &str,
        held: Option<Selected>,
    ) -> Result<(Arc<Account>, String, Option<Selected>), SendError> {
        let selected = held.ok_or(SendError::Revoked)?;
        credential.check()?;
        let scope = format!("sms:{}", identity.owner.account);
        service.publish(
            identity,
            crate::door::Publication {
                account: identity.owner.account.clone(),
                name: name.into(),
                generation: identity.owner.linking_generation,
                kind: crate::door::PublicationKind::Sms {
                    scope: scope.clone(),
                },
            },
        )?;
        let account = service.account(identity, credential, &scope, Account::new)?;
        let root = service.private_root(&identity.owner)?;
        tokio::runtime::Handle::current().block_on(account.activate(
            selected,
            root,
            scope.clone(),
        ))?;
        Ok((account, scope, None))
    }

    fn resume(
        &self,
        service: &Arc<super::Service>,
        identity: &Identity,
        credential: super::identity::Credential,
        scope: String,
        _row: &accounts::Account,
    ) -> Result<Arc<Account>, SendError> {
        let account = service.account(identity, credential, &scope, Account::new)?;
        let root = service.private_root(&identity.owner)?;
        let resuming = account.clone();
        tokio::runtime::Handle::current().block_on(async move {
            let connection = connection().await?;
            tokio::time::timeout(
                Duration::from_secs(10),
                resuming.resume(connection, root, scope),
            )
            .await
            .map_err(|_| SendError::Unavailable)?
        })?;
        Ok(account)
    }
}
