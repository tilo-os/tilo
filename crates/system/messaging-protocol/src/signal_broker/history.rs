//! Encrypted history owned by one persona/device/account/link generation.
use crate::journal::Owner;
use advisory_lock::FileLock;
use secret_vault::{VaultFormat, VaultKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tilo_contact_directory::routing::Route;
use tilo_messaging::{Delivery, Direction, HistoryPage, Message, SendError};
use zeroize::{Zeroize, Zeroizing};
#[path = "history/groups.rs"]
pub(crate) mod groups;
pub(crate) use groups::GroupIncoming;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ROWS: usize = 16_384;
const FORMAT: VaultFormat = VaultFormat {
    name: "tilo.messaging-history.v1",
    magic: b"TMHI",
    secret_service_label: "Tilo messaging history",
    secret_service_attributes: &[
        ("application", "so.tilo.messaging-broker"),
        ("purpose", "history"),
    ],
    dpapi_entropy: b"tilo.messaging-history.v1",
};
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Address {
    person: String,
    association: String,
    sender: String,
    service: String,
    scope: String,
    recipient: String,
    revision: String,
}
impl From<&Route> for Address {
    fn from(r: &Route) -> Self {
        Self {
            person: r.person.as_str().into(),
            association: r.association.as_str().into(),
            sender: r.sender.as_str().into(),
            service: r.service.clone(),
            scope: r.scope.clone(),
            recipient: r.recipient.clone(),
            revision: r.revision.clone(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    address: Address,
    text: String,
    timestamp: u64,
    incoming: bool,
    acknowledged: bool,
    #[serde(default)]
    submitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recipient_evidence: Option<crate::journal::RecipientEvidence>,
}
impl Drop for Row {
    fn drop(&mut self) {
        self.text.zeroize();
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    owner: Owner,
    rows: BTreeMap<String, Row>,
    #[serde(default)]
    groups: BTreeMap<String, GroupIncoming>,
    #[serde(default)]
    pending: Vec<Vec<u8>>,
}
impl Drop for Ledger {
    fn drop(&mut self) {
        self.pending.zeroize();
    }
}

#[cfg(feature = "whatsapp-sdk")]
pub(crate) struct IncomingRecord<'a> {
    pub route: &'a Route,
    pub id: &'a str,
    pub text: &'a str,
    pub timestamp_ms: u64,
}

pub struct History {
    path: PathBuf,
    key: VaultKey,
    ledger: Ledger,
    usable: bool,
    _lock: FileLock,
}
impl History {
    /// Open private ciphertext under existing host-owned custody; never reset.
    /// # Errors
    /// Refuses competing owners, corrupt/oversized state and wrong authority.
    pub fn open(root: &Path, owner: &Owner, key: VaultKey) -> Result<Self, SendError> {
        if !owner.valid_principal()
            || !matches!(
                owner.service,
                crate::Service::Tilo
                    | crate::Service::Signal
                    | crate::Service::Whatsapp
                    | crate::Service::Sms
            )
            || !crate::identifier(&owner.device)
            || !crate::identifier(&owner.account)
        {
            return Err(SendError::Unavailable);
        }
        private_fs::directory(root).map_err(|_| SendError::Unavailable)?;
        let file = private_fs::lock_file(&root.join("history.lock"))
            .map_err(|_| SendError::Unavailable)?;
        let lock = FileLock::try_exclusive(file).map_err(|_| SendError::Unavailable)?;
        let path = root.join("history.enc");
        let mut created = false;
        let ledger = match private_fs::open_owned_for_read(&path, (MAX_BYTES + 1024) as u64) {
            Ok((mut file, _)) => {
                use std::io::Read;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)
                    .map_err(|_| SendError::Unavailable)?;
                let plain = FORMAT.open(&key, &bytes).ok_or(SendError::Unavailable)?;
                let plain = Zeroizing::new(plain);
                serde_json::from_slice::<Ledger>(&plain).map_err(|_| SendError::Unavailable)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                created = true;
                Ledger {
                    owner: owner.clone(),
                    rows: BTreeMap::new(),
                    groups: BTreeMap::new(),
                    pending: Vec::new(),
                }
            }
            Err(_) => return Err(SendError::Unavailable),
        };
        if &ledger.owner != owner
            || ledger.rows.len().saturating_add(ledger.groups.len()) > MAX_ROWS
            || (!ledger.groups.is_empty() && owner.service != crate::Service::Whatsapp)
            || ledger.groups.iter().any(|(key, row)| {
                !row.valid() || !matches!(row.key(),Ok(ref expected) if expected==key)
            })
            || ledger.pending.len() > 64
            || ledger.pending.iter().map(Vec::len).sum::<usize>() > 4 * 1024 * 1024
            || ledger.rows.values().any(|r| {
                r.text.len() > 32 * 1024
                    || r.timestamp == 0
                    || r.address.service != service_name(owner.service)
                    || r.address.sender != owner.account
                    || (r.recipient_evidence.is_some()
                        && (!matches!(
                            owner.service,
                            crate::Service::Tilo
                                | crate::Service::Whatsapp
                                | crate::Service::Signal
                        ) || r.incoming))
            })
        {
            return Err(SendError::Unavailable);
        }
        let mut history = Self {
            path,
            key,
            ledger,
            usable: true,
            _lock: lock,
        };
        if created {
            history.persist()?;
        }
        Ok(history)
    }
    pub(crate) fn owner(&self) -> &Owner {
        &self.ledger.owner
    }
    fn persist(&mut self) -> Result<(), SendError> {
        let result = (|| {
            let bytes = Zeroizing::new(
                serde_json::to_vec(&self.ledger).map_err(|_| SendError::Unavailable)?,
            );
            if bytes.len() > MAX_BYTES {
                return Err(SendError::Unavailable);
            }
            let sealed = FORMAT
                .seal(&self.key, &bytes)
                .ok_or(SendError::Unavailable)?;
            private_fs::write_atomic(&self.path, &sealed).map_err(|_| SendError::Unavailable)
        })();
        if result.is_err() {
            self.usable = false;
        }
        result
    }
    pub(crate) fn stage_events(&mut self, events: &[Zeroizing<Vec<u8>>]) -> Result<(), SendError> {
        if !self.usable
            || self.ledger.pending.len().saturating_add(events.len()) > 64
            || self
                .ledger
                .pending
                .iter()
                .map(Vec::len)
                .sum::<usize>()
                .saturating_add(events.iter().map(|e| e.len()).sum::<usize>())
                > 4 * 1024 * 1024
        {
            return Err(SendError::Unavailable);
        }
        if events.is_empty() {
            return Ok(());
        }
        self.ledger
            .pending
            .extend(events.iter().map(|e| e.to_vec()));
        self.persist()
    }
    pub(crate) fn stage_incoming(
        &mut self,
        account: &str,
        incoming: &crate::signal::Incoming,
    ) -> Result<(), SendError> {
        let value = serde_json::json!({"jsonrpc":"2.0","method":"receive","params":{"account":account,"envelope":{"sourceUuid":incoming.id.sender_aci,"dataMessage":{"timestamp":incoming.id.timestamp,"message":incoming.text.as_str()}}}});
        let bytes = Zeroizing::new(serde_json::to_vec(&value).map_err(|_| SendError::Unavailable)?);
        self.stage_events(&[bytes])
    }
    pub(crate) fn pending(&self) -> Vec<Zeroizing<Vec<u8>>> {
        self.ledger
            .pending
            .iter()
            .cloned()
            .map(Zeroizing::new)
            .collect()
    }
    pub(crate) fn retain_pending(
        &mut self,
        events: &[Zeroizing<Vec<u8>>],
    ) -> Result<(), SendError> {
        if !self.usable {
            return Err(SendError::Unavailable);
        }
        if events.len() == self.ledger.pending.len() {
            return Ok(());
        }
        self.ledger.pending.zeroize();
        self.ledger.pending = events.iter().map(|e| e.to_vec()).collect();
        self.persist()
    }
    pub(crate) fn check_operation(&self, operation: &str) -> Result<(), SendError> {
        if !self.usable {
            return Err(SendError::Unavailable);
        }
        if self.ledger.rows.contains_key(&format!("out:{operation}")) {
            return Err(SendError::AlreadyReserved);
        }
        Ok(())
    }
    pub(crate) fn reserve(
        &mut self,
        route: &Route,
        operation: &str,
        text: &str,
    ) -> Result<(), SendError> {
        self.insert(
            route,
            format!("out:{operation}"),
            text,
            tilo_clock::unix_now_ms(),
            false,
        )
    }
    pub(crate) fn incoming(
        &mut self,
        route: &Route,
        id: &str,
        text: &str,
        time: u64,
    ) -> Result<(), SendError> {
        self.incoming_changed(route, id, text, time).map(|_| ())
    }
    pub(crate) fn incoming_changed(
        &mut self,
        route: &Route,
        id: &str,
        text: &str,
        time: u64,
    ) -> Result<bool, SendError> {
        let key = format!("in:{id}");
        if let Some(existing) = self.ledger.rows.get(&key) {
            return if self.usable
                && existing.address == Address::from(route)
                && existing.text == text
                && existing.timestamp == time
            {
                Ok(false)
            } else {
                Err(SendError::InvalidRequest)
            };
        }
        self.insert(route, key, text, time, true).map(|()| true)
    }
    /// Stage a bounded batch, then publish exactly one encrypted atomic write.
    /// Validation failure cannot expose a partially accepted batch.
    #[cfg(feature = "whatsapp-sdk")]
    pub(crate) fn incoming_batch(
        &mut self,
        batch: &[IncomingRecord<'_>],
    ) -> Result<usize, SendError> {
        self.incoming_mixed(batch, &[])
    }
    #[cfg(feature = "whatsapp-sdk")]
    fn stage_direct(
        &self,
        batch: &[IncomingRecord<'_>],
    ) -> Result<BTreeMap<String, Row>, SendError> {
        let mut staged = BTreeMap::<String, Row>::new();
        let mut bytes = 0usize;
        for incoming in batch {
            bytes = bytes.saturating_add(incoming.text.len());
            if incoming.id.len() > 509
                || incoming.text.len() > 32 * 1024
                || incoming.text.trim().is_empty()
                || incoming.timestamp_ms == 0
                || bytes > 256 * 1024
            {
                return Err(SendError::InvalidRequest);
            }
            let key = format!("in:{}", incoming.id);
            let address = Address::from(incoming.route);
            if let Some(existing) = self.ledger.rows.get(&key).or_else(|| staged.get(&key)) {
                if existing.address != address
                    || existing.text != incoming.text
                    || existing.timestamp != incoming.timestamp_ms
                    || !existing.incoming
                {
                    return Err(SendError::InvalidRequest);
                }
                continue;
            }
            staged.insert(
                key,
                Row {
                    address,
                    text: incoming.text.into(),
                    timestamp: incoming.timestamp_ms,
                    incoming: true,
                    acknowledged: false,
                    submitted: false,
                    recipient_evidence: None,
                },
            );
        }
        Ok(staged)
    }
    fn insert(
        &mut self,
        route: &Route,
        id: String,
        text: &str,
        time: u64,
        incoming: bool,
    ) -> Result<(), SendError> {
        if !self.usable
            || self
                .ledger
                .rows
                .len()
                .saturating_add(self.ledger.groups.len())
                >= MAX_ROWS
            || text.len() > 32 * 1024
            || time == 0
        {
            return Err(SendError::Unavailable);
        }
        if self.ledger.rows.contains_key(&id) {
            return Err(SendError::AlreadyReserved);
        }
        self.ledger.rows.insert(
            id,
            Row {
                address: Address::from(route),
                text: text.into(),
                timestamp: time,
                incoming,
                acknowledged: false,
                submitted: false,
                recipient_evidence: None,
            },
        );
        self.persist()
    }
    #[cfg(any(feature = "whatsapp-sdk", feature = "sms-dbus"))]
    pub(crate) fn submitted(&mut self, operation: &str) -> Result<(), SendError> {
        if !self.usable {
            return Err(SendError::Uncertain);
        }
        let row = self
            .ledger
            .rows
            .get_mut(&format!("out:{operation}"))
            .ok_or(SendError::Uncertain)?;
        row.submitted = true;
        self.persist().map_err(|_| SendError::Uncertain)
    }
    pub(crate) fn acknowledge(&mut self, operation: &str, time: u64) -> Result<(), SendError> {
        if !self.usable || time == 0 {
            return Err(SendError::Uncertain);
        }
        let row = self
            .ledger
            .rows
            .get_mut(&format!("out:{operation}"))
            .ok_or(SendError::Uncertain)?;
        row.timestamp = time;
        row.acknowledged = true;
        self.persist().map_err(|_| SendError::Uncertain)
    }
    pub(crate) fn outgoing_route(&self, operation: &str) -> Result<Route, SendError> {
        if !self.usable || !crate::identifier(operation) {
            return Err(SendError::Unavailable);
        }
        let address = &self
            .ledger
            .rows
            .get(&format!("out:{operation}"))
            .ok_or(SendError::InvalidRequest)?
            .address;
        Ok(Route {
            person: tilo_resource::Id::from_raw(&address.person),
            association: tilo_resource::Id::from_raw(&address.association),
            sender: tilo_resource::Id::from_raw(&address.sender),
            service: address.service.clone(),
            scope: address.scope.clone(),
            recipient: address.recipient.clone(),
            revision: address.revision.clone(),
        })
    }
    pub(crate) fn observe_recipient(
        &mut self,
        route: &Route,
        operation: &str,
        evidence: crate::journal::RecipientEvidence,
    ) -> Result<bool, SendError> {
        if !matches!(
            self.ledger.owner.service,
            crate::Service::Tilo | crate::Service::Whatsapp | crate::Service::Signal
        ) || self.outgoing_route(operation)? != *route
        {
            return Err(SendError::Revoked);
        }
        let row = self
            .ledger
            .rows
            .get_mut(&format!("out:{operation}"))
            .ok_or(SendError::InvalidRequest)?;
        if row.recipient_evidence.is_some_and(|old| old >= evidence) {
            return Ok(false);
        }
        row.recipient_evidence = Some(evidence);
        self.persist().map_err(|_| SendError::Uncertain)?;
        Ok(true)
    }
    pub(crate) fn page(&self, route: Route, limit: usize) -> Result<HistoryPage, SendError> {
        if !self.usable {
            return Err(SendError::Unavailable);
        }
        let address = Address::from(&route);
        let mut rows: Vec<_> = self
            .ledger
            .rows
            .iter()
            .filter(|(_, r)| r.address == address)
            .collect();
        rows.sort_by_key(|(id, r)| (r.timestamp, *id));
        let more = rows.len() > limit;
        let skip = rows.len().saturating_sub(limit);
        let retained = rows.len().saturating_sub(skip);
        let mut bytes = 0usize;
        let messages = rows
            .into_iter()
            .skip(skip)
            .rev()
            .take_while(|(id, r)| {
                bytes = bytes.saturating_add(id.len()).saturating_add(r.text.len());
                bytes <= 240 * 1024
            })
            .map(|(id, r)| Message {
                id: id.clone(),
                text: Zeroizing::new(r.text.clone()),
                timestamp_ms: r.timestamp,
                direction: if r.incoming {
                    Direction::Incoming
                } else {
                    Direction::Outgoing
                },
                delivery: if r.incoming {
                    None
                } else {
                    Some(match r.recipient_evidence {
                        Some(crate::journal::RecipientEvidence::Read) => Delivery::RecipientRead,
                        Some(crate::journal::RecipientEvidence::Delivered) => {
                            Delivery::RecipientDelivered
                        }
                        None if r.acknowledged => Delivery::ServerAcknowledged,
                        None if r.submitted => Delivery::Submitted,
                        None => Delivery::Uncertain,
                    })
                },
            })
            .collect::<Vec<_>>();
        let mut messages = messages;
        messages.reverse();
        let more = more || messages.len() < retained;
        Ok(HistoryPage {
            route,
            messages,
            more,
        })
    }
}

pub(crate) fn service_name(service: crate::Service) -> &'static str {
    match service {
        crate::Service::Signal => "signal",
        crate::Service::Whatsapp => "whatsapp",
        crate::Service::Sms => "sms",
        crate::Service::Tilo => "tilo",
    }
}
