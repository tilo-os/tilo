//! Bounded inert creation custody shared by the existing governor owners.
//! These values never authorize an ingress, sign a document or prove durability.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tilo_user_identity::DeviceCert;

pub const MAX_CREATION_ROWS: usize = 65_536;
pub const MAX_CREATION_LEDGER_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CREATION_SOURCE_BYTES: usize = 65_536;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CreationKeyV1 {
    pub room: [u8; 16],
    pub caller_root: [u8; 32],
    pub caller_device: [u8; 32],
    pub invocation: [u8; 16],
}
/// Retained ownership observations only. The ingress must resolve live authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetainedInvocationScope {
    Certified {
        root: [u8; 32],
        device: [u8; 32],
    },
    Enrolled {
        root: [u8; 32],
        device: [u8; 32],
        task: [u8; 16],
        run: [u8; 16],
        channel: [u8; 16],
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicTargetV1 {
    New,
    Primary,
    ExactUuid {
        #[serde(deserialize_with = "short_string")]
        record: String,
    },
}
/// No public constructor: only a future source-bound native door can mint it.
/// Decoding this historical value cannot establish current channel custody.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeBoundTargetV1(NativeTarget);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum NativeTarget {
    Primary,
    ExactUuid {
        #[serde(deserialize_with = "short_string")]
        record: String,
    },
    ExactTab {
        #[serde(deserialize_with = "short_string")]
        raw_record: String,
        tab: u64,
        workspace: u64,
        namespace_owner_device: [u8; 32],
        original_new_identity_digest: [u8; 32],
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CreationSourceV1 {
    PublicAction {
        original_action_digest: [u8; 32],
        owner_scope: RetainedInvocationScope,
        target: PublicTargetV1,
    },
    DesktopFactory {
        original_desktop_digest: [u8; 32],
        owner_scope: RetainedInvocationScope,
    },
    NativeDraft {
        original_envelope_digest: [u8; 32],
        target: NativeBoundTargetV1,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationIntentV1 {
    pub version: u16,
    pub key: CreationKeyV1,
    pub original_caller_certificate_digest: [u8; 32],
    #[serde(deserialize_with = "short_string")]
    pub app: String,
    pub registry_revision: [u8; 32],
    pub source: CreationSourceV1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedCreationRouteV1 {
    pub room: [u8; 16],
    #[serde(deserialize_with = "short_string")]
    pub app: String,
    #[serde(deserialize_with = "short_string")]
    pub record: String,
    #[serde(deserialize_with = "short_string")]
    pub kind: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedV1 {
    pub genesis_payload_hash: [u8; 32],
    pub winning_signed_envelope_digest: [u8; 32],
    pub publication_author: [u8; 32],
    pub publication_sequence: u64,
    pub exact_admission_identity: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictV1 {
    pub observed_canonical_revision: [u8; 32],
    pub observed_checkpoint_digest: [u8; 32],
    pub known_winner_result_id: Option<[u8; 32]>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CreationTerminalV1 {
    Created(CreatedV1),
    ExistingConflict(ConflictV1),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationResultBodyV1 {
    pub version: u16,
    pub key: CreationKeyV1,
    pub intent_digest: [u8; 32],
    pub route: ResolvedCreationRouteV1,
    pub terminal: CreationTerminalV1,
    pub owner_generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationResultV1 {
    pub body: CreationResultBodyV1,
    pub business_binding: [u8; 32],
    pub result_id: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationRowV1 {
    pub intent: CreationIntentV1,
    pub original_certificate: DeviceCert,
    /// Canonical original owner/action metadata, not a duplicate native draft.
    #[serde(deserialize_with = "source_bytes")]
    pub original_request: Vec<u8>,
    pub result: CreationResultV1,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreationLedgerV1 {
    rows: BTreeMap<CreationKeyV1, CreationRowV1>,
}

impl CreationLedgerV1 {
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn rows(&self) -> impl ExactSizeIterator<Item = &CreationRowV1> {
        self.rows.values()
    }
    pub fn lookup(&self, key: &CreationKeyV1) -> Option<&CreationRowV1> {
        self.rows.get(key)
    }
    /// Inserts inert custody only; the real owner must persist the whole candidate.
    /// Inert storage preflight. Uses a maximum-width terminal/route shape only
    /// for counting; it neither inserts a row nor creates an outcome.
    pub fn reserve_row(
        &self,
        intent: &CreationIntentV1,
        certificate: &DeviceCert,
        original: &[u8],
    ) -> Result<(), String> {
        if self.rows.len() >= MAX_CREATION_ROWS
            || original.is_empty()
            || original.len() > MAX_CREATION_SOURCE_BYTES
        {
            return Err("creation ledger/source capacity".into());
        }
        let body = CreationResultBodyV1 {
            version: 1,
            key: intent.key.clone(),
            intent_digest: intent.digest()?,
            route: ResolvedCreationRouteV1 {
                room: intent.key.room,
                app: intent.app.clone(),
                record: "x".repeat(128),
                kind: "x".repeat(128),
            },
            terminal: CreationTerminalV1::Created(CreatedV1 {
                genesis_payload_hash: [0; 32],
                winning_signed_envelope_digest: [0; 32],
                publication_author: [0; 32],
                publication_sequence: u64::MAX,
                exact_admission_identity: [0; 32],
            }),
            owner_generation: u64::MAX,
        };
        let size_only = CreationRowV1 {
            intent: intent.clone(),
            original_certificate: certificate.clone(),
            original_request: original.to_vec(),
            result: CreationResultV1::from_body(body)?,
        };
        codec::count_rows(
            self.rows.values().chain(std::iter::once(&size_only)),
            self.rows.len() + 1,
        )
        .map(|_| ())
    }
    pub fn admit(&mut self, row: CreationRowV1) -> Result<(), String> {
        row.validate()?;
        if let Some(prior) = self.rows.get(&row.intent.key) {
            return if prior == &row {
                Ok(())
            } else {
                Err("creation identity substitution".into())
            };
        }
        if self.rows.len() >= MAX_CREATION_ROWS
            || self.rows.keys().any(|key| {
                key.room == row.intent.key.room
                    && key.caller_device == row.intent.key.caller_device
                    && key.invocation == row.intent.key.invocation
            })
        {
            return Err("creation capacity or invocation collision".into());
        }
        codec::count_rows(
            self.rows.values().chain(std::iter::once(&row)),
            self.rows.len() + 1,
        )?;
        self.rows.insert(row.intent.key.clone(), row);
        Ok(())
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.rows.len() > MAX_CREATION_ROWS {
            return Err("creation row limit".into());
        }
        let mut scopes = std::collections::BTreeSet::new();
        for (key, row) in &self.rows {
            row.validate()?;
            if key != &row.intent.key
                || !scopes.insert((key.room, key.caller_device, key.invocation))
            {
                return Err("creation key collision".into());
            }
        }
        self.encoded_len().map(|_| ())
    }
    pub fn encoded_len(&self) -> Result<usize, String> {
        codec::count_rows(self.rows.values(), self.rows.len())
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        codec::encode(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        codec::decode(bytes)
    }
}
impl CreationIntentV1 {
    pub fn digest(&self) -> Result<[u8; 32], String> {
        digest("qOS document creation intent v1", self)
    }
}
impl CreationResultV1 {
    pub fn from_body(body: CreationResultBodyV1) -> Result<Self, String> {
        let business_binding = digest(
            "qOS creation business result v1",
            &(body.intent_digest, &body.route, &body.terminal),
        )?;
        let result_id = digest(
            "qOS document creation result v1",
            &(&body, business_binding),
        )?;
        Ok(Self {
            body,
            business_binding,
            result_id,
        })
    }
}
pub fn certificate_digest(certificate: &DeviceCert) -> Result<[u8; 32], String> {
    digest("qOS creation caller certificate v1", certificate)
}
fn digest(domain: &str, value: &impl Serialize) -> Result<[u8; 32], String> {
    Ok(blake3::derive_key(
        domain,
        &postcard::to_stdvec(value).map_err(|e| e.to_string())?,
    ))
}
fn short_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = <&str>::deserialize(d)?;
    if s.len() > 128 {
        return Err(serde::de::Error::custom("creation string limit"));
    }
    Ok(s.to_owned())
}
fn source_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let s = <&[u8]>::deserialize(d)?;
    if s.len() > MAX_CREATION_SOURCE_BYTES {
        return Err(serde::de::Error::custom("creation source limit"));
    }
    Ok(s.to_vec())
}
mod codec;
#[cfg(test)]
mod tests;

impl CreationKeyV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.room == [0; 16]
            || self.invocation == [0; 16]
            || self.caller_root == [0; 32]
            || self.caller_device == [0; 32]
        {
            return Err("invalid creation identity".into());
        }
        Ok(())
    }
}
impl CreationRowV1 {
    pub fn validate(&self) -> Result<(), String> {
        let intent = &self.intent;
        intent.key.validate()?;
        if intent.version != 1
            || self.result.body.version != 1
            || self.result.body.owner_generation == 0
            || self.original_request.is_empty()
            || self.original_request.len() > MAX_CREATION_SOURCE_BYTES
            || self.original_certificate.user_public_key != intent.key.caller_root
            || self.original_certificate.device_public_key != intent.key.caller_device
            || certificate_digest(&self.original_certificate)?
                != intent.original_caller_certificate_digest
            || self.result.body.key != intent.key
            || self.result.body.intent_digest != intent.digest()?
            || self.result.body.route.room != intent.key.room
            || self.result.body.route.app != intent.app
            || CreationResultV1::from_body(self.result.body.clone())? != self.result
        {
            return Err("invalid creation binding".into());
        }
        label(&intent.app)?;
        label(&self.result.body.route.kind)?;
        route(&self.result.body.route.record)?;
        match &intent.source {
            CreationSourceV1::PublicAction {
                owner_scope,
                target,
                ..
            } => {
                scope(owner_scope, &intent.key)?;
                if let PublicTargetV1::ExactUuid { record } = target {
                    exact_uuid(record)?;
                }
            }
            CreationSourceV1::DesktopFactory {
                original_desktop_digest,
                owner_scope,
            } => {
                scope(owner_scope, &intent.key)?;
                let (request, trailing) = postcard::take_from_bytes::<
                    qos_peer_state::owner_commit::OwnerCommitRequest,
                >(&self.original_request)
                .map_err(|e| e.to_string())?;
                if !trailing.is_empty()
                    || postcard::to_stdvec(&request).map_err(|e| e.to_string())?
                        != self.original_request
                    || request.binding()?.request_digest != *original_desktop_digest
                    || request.room != uuid::Uuid::from_bytes(intent.key.room).to_string()
                {
                    return Err("invalid original desktop intent".into());
                }
                let qos_peer_state::owner_commit::CommitOperation::DesktopCommand {
                    invocation,
                    caller,
                    command,
                } = &request.operation
                else {
                    return Err("invalid original desktop operation".into());
                };
                if invocation.as_bytes() != &intent.key.invocation
                    || caller != &self.original_certificate
                {
                    return Err("invalid original desktop caller".into());
                }
                match &command.command {
                    qos_peer_state::desktop_command::DesktopCommand::OpenTab { app, .. }
                    | qos_peer_state::desktop_command::DesktopCommand::OpenApp { app }
                        if app == &intent.app =>
                    {
                        ()
                    }
                    qos_peer_state::desktop_command::DesktopCommand::DuplicateTab { .. } => (),
                    _ => return Err("original desktop command has no checked creation".into()),
                }
            }
            CreationSourceV1::NativeDraft { target, .. } => match &target.0 {
                NativeTarget::Primary => (),
                NativeTarget::ExactUuid { record } => exact_uuid(record)?,
                NativeTarget::ExactTab {
                    raw_record,
                    tab,
                    workspace,
                    namespace_owner_device,
                    original_new_identity_digest,
                } => {
                    if *tab == 0
                        || *workspace == 0
                        || *namespace_owner_device == [0; 32]
                        || *original_new_identity_digest == [0; 32]
                        || raw_record != &format!("tab:{tab:016x}")
                    {
                        return Err("invalid retained native tab identity".into());
                    }
                }
            },
        }
        if let CreationTerminalV1::Created(created) = &self.result.body.terminal {
            if created.publication_sequence == 0 || created.publication_author == [0; 32] {
                return Err("invalid creation publication identity".into());
            }
        }
        Ok(())
    }
}
fn label(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value.is_ascii()
        || value.trim() != value
        || value.bytes().any(|c| c.is_ascii_control())
    {
        return Err("invalid creation label".into());
    }
    Ok(())
}
fn exact_uuid(record: &str) -> Result<(), String> {
    let text = record
        .strip_prefix("uuid:")
        .ok_or("creation record must be canonical UUID")?;
    let id = uuid::Uuid::parse_str(text).map_err(|e| e.to_string())?;
    if id.is_nil() || id.to_string() != text {
        return Err("creation record must be canonical UUID".into());
    }
    Ok(())
}
fn route(record: &str) -> Result<(), String> {
    if record.is_empty() {
        return Ok(());
    }
    if let Some(raw) = record.strip_prefix("tab:") {
        let value = u64::from_str_radix(raw, 16).map_err(|e| e.to_string())?;
        if value == 0 || record != format!("tab:{value:016x}") {
            return Err("invalid canonical tab route".into());
        }
        return Ok(());
    }
    exact_uuid(record)
}
fn scope(scope: &RetainedInvocationScope, key: &CreationKeyV1) -> Result<(), String> {
    let (root, device) = match scope {
        RetainedInvocationScope::Certified { root, device } => (root, device),
        RetainedInvocationScope::Enrolled {
            root,
            device,
            task,
            run,
            channel,
        } => {
            if *task == [0; 16] || *run == [0; 16] || *channel == [0; 16] {
                return Err("invalid retained enrolled scope".into());
            }
            (root, device)
        }
    };
    if root != &key.caller_root || device != &key.caller_device {
        return Err("creation ownership mismatch".into());
    }
    Ok(())
}
