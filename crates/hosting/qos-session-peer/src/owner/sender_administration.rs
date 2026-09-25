//! Explicit administrative storage selection. No selected owner or runtime
//! capability is returned by this API.
use super::{Identity, SessionOwner, SignedAggregate, schema};
use anyhow::Result;
use atomic_file::{ExistingCommitError, PublicationError, bounded::BoundedOwnerDirectory};
use ed25519_dalek::Signer;
use serde::Serialize;
use std::{ffi::OsStr, path::Path};
use ui::{
    automerge::resource_limits::{NativeAllowance, NativeReservation},
    host_worker::private_actions::{
        SenderScope, SenderStorageCode,
        administration::{AdministrativeExpectation, AdministrativeFailure, AdministrativeFile},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminPhase {
    Prepared,
    Active,
}

/// Inert administrative facts, never a storage, owner, or send capability.
#[derive(Debug, Serialize)]
pub struct AdminStatus {
    pub phase: AdminPhase,
    pub room: [u8; 16],
    pub owner_root: [u8; 32],
    pub native_device: [u8; 32],
    pub instance: [u8; 16],
}

/// Refusal retains the original operation and any actual physical failure.
/// There is no public owner, receipt, retry authority, or success constructor.
/// A separate explicit resume authenticates and proves disk state anew.
pub struct AdminError {
    cause: Cause,
    operation: Option<Operation>,
}
#[derive(Debug)]
enum Cause {
    Ordinary(anyhow::Error),
    Physical,
    Refused(&'static str),
    Code(SenderStorageCode),
    Io(std::io::Error),
    Admission(atomic_file::bounded::AdmissionError),
    Child(atomic_file::bounded::ChildOpenError),
    Resource(ui::automerge::resource_limits::NativeResourceError),
    Encoding(postcard::Error),
    Allocation(std::collections::TryReserveError),
}
macro_rules! inline_cause {
    ($ty:ty, $variant:ident) => {
        impl From<$ty> for Cause {
            fn from(error: $ty) -> Self {
                Self::$variant(error)
            }
        }
    };
}
inline_cause!(std::io::Error, Io);
inline_cause!(atomic_file::bounded::AdmissionError, Admission);
inline_cause!(atomic_file::bounded::ChildOpenError, Child);
inline_cause!(
    ui::automerge::resource_limits::NativeResourceError,
    Resource
);
inline_cause!(postcard::Error, Encoding);
inline_cause!(std::collections::TryReserveError, Allocation);

impl From<anyhow::Error> for Cause {
    fn from(error: anyhow::Error) -> Self {
        Self::Ordinary(error)
    }
}
impl From<anyhow::Error> for AdminError {
    fn from(error: anyhow::Error) -> Self {
        Self {
            cause: Cause::Ordinary(error),
            operation: None,
        }
    }
}
impl AdminError {
    pub fn storage_code(&self) -> Option<SenderStorageCode> {
        self.operation
            .as_ref()?
            .physical_failure
            .as_ref()
            .map(AdministrativeFailure::code)
    }
    pub fn publication_error(&self) -> Option<&PublicationError> {
        match &self.cause {
            Cause::Ordinary(e) => e.downcast_ref(),
            Cause::Physical => self
                .operation
                .as_ref()?
                .physical_failure
                .as_ref()?
                .publication_error(),
            _ => None,
        }
    }
    pub fn existing_commit_error(&self) -> Option<&ExistingCommitError> {
        self.operation
            .as_ref()?
            .physical_failure
            .as_ref()?
            .existing_commit_error()
    }
}
impl std::fmt::Debug for AdminError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminError")
            .field("message", &format_args!("{self}"))
            .field("storage_code", &self.storage_code())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for AdminError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.cause {
            Cause::Ordinary(e) => std::fmt::Display::fmt(e, f),
            Cause::Physical => write!(
                f,
                "administrative physical storage refused: {:?}",
                self.storage_code()
            ),
            Cause::Refused(message) => f.write_str(message),
            Cause::Code(code) => write!(f, "administrative storage refused: {code:?}"),
            Cause::Io(e) => std::fmt::Display::fmt(e, f),
            Cause::Admission(e) => std::fmt::Display::fmt(e, f),
            Cause::Child(e) => std::fmt::Display::fmt(e, f),
            Cause::Resource(e) => std::fmt::Display::fmt(e, f),
            Cause::Encoding(e) => std::fmt::Display::fmt(e, f),
            Cause::Allocation(e) => std::fmt::Display::fmt(e, f),
        }
    }
}
impl std::error::Error for AdminError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.cause {
            Cause::Ordinary(e) => Some(e.as_ref()),
            Cause::Refused(_) | Cause::Code(_) => None,
            Cause::Io(e) => Some(e),
            Cause::Admission(e) => Some(e),
            Cause::Child(e) => Some(e),
            Cause::Resource(e) => Some(e),
            Cause::Encoding(e) => Some(e),
            Cause::Allocation(e) => Some(e),
            Cause::Physical => {
                let e = self.operation.as_ref()?.physical_failure.as_ref()?;
                if let Some(e) = e.publication_error() {
                    return Some(e);
                }
                if let Some(e) = e.existing_commit_error() {
                    return Some(e);
                }
                if let Some(e) = e.fresh_child_error() {
                    return Some(e);
                }
                e.io_error()
                    .map(|e| e as &(dyn std::error::Error + 'static))
            }
        }
    }
}

/// The actual CLI transports this error by value, including its owned native
/// refusal. In particular it must never box AdminError inside anyhow::Error.
pub enum CliError {
    Ordinary(anyhow::Error),
    Administration(AdminError),
    Json(serde_json::Error),
    Io(std::io::Error),
}
pub type CliResult<T> = std::result::Result<T, CliError>;
impl From<AdminError> for CliError {
    fn from(error: AdminError) -> Self {
        Self::Administration(error)
    }
}
impl From<anyhow::Error> for CliError {
    fn from(error: anyhow::Error) -> Self {
        Self::Ordinary(error)
    }
}
impl From<serde_json::Error> for CliError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl std::fmt::Debug for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ordinary(e) => std::fmt::Debug::fmt(e, f),
            Self::Administration(e) => std::fmt::Debug::fmt(e, f),
            Self::Json(e) => std::fmt::Display::fmt(e, f),
            Self::Io(e) => std::fmt::Display::fmt(e, f),
        }
    }
}

/// One actual ingress inventory, retained for this administrative invocation.
/// Ordinary identity/owner admission and its native heaps remain unqualified.
struct AdminInventory {
    allowance: NativeAllowance,
    _ingress: crate::https::Ingress,
    _receiver: crate::https::IngressReceiver,
}
impl AdminInventory {
    fn new() -> Result<Self> {
        let (ingress, receiver) = crate::https::try_ingress_channel()?;
        let allowance = receiver.host_bytes().native_allowance()?;
        Ok(Self {
            allowance,
            _ingress: ingress,
            _receiver: receiver,
        })
    }
}
impl Drop for AdminInventory {
    fn drop(&mut self) {
        self.allowance.finish_turn();
    }
}

// Backing precedes its original reservation on every success/error drop.
struct Encoded {
    _payload: Vec<u8>,
    signed: Vec<u8>,
    _credit: NativeReservation,
}
#[derive(Serialize)]
struct SignedView<'a> {
    payload: &'a [u8],
    signature: &'a [u8],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Intent {
    Select,
    Resume,
    Status,
}
struct Operation {
    // Physical errors and backing must die before their credits and inventory.
    output: Option<Encoded>,
    physical_failure: Option<AdministrativeFailure>,
    physical: Option<AdministrativeFile>,
    physical_credit: Option<NativeReservation>,
    store: crate::store::SessionStore,
    identity: Identity,
    original: Vec<u8>,
    signed: SignedAggregate,
    selection: Option<schema::SenderSelection>,
    room: uuid::Uuid,
    generation: u64,
    inventory: AdminInventory,
}
impl Operation {
    fn admit(
        store: crate::store::SessionStore,
        identity: Identity,
        inventory: AdminInventory,
    ) -> Result<Self> {
        let original = store.read()?;
        let signed: SignedAggregate = postcard::from_bytes(&original)?;
        schema::canonical(&signed, &original)?;
        // This consumes only the existing version-specific owner decoder. No
        // additional document/body parser or alternative native state exists.
        let aggregate = schema::Aggregate::decode_administrative_payload(&signed.payload)?;
        let _authenticated = super::restore(&aggregate, &signed, &identity)?;
        let room = uuid::Uuid::parse_str(&aggregate.room)?;
        Ok(Self {
            output: None,
            physical_failure: None,
            physical: None,
            physical_credit: None,
            store,
            identity,
            original,
            signed,
            selection: aggregate.sender,
            room,
            generation: aggregate.generation,
            inventory,
        })
    }
    fn existing(root: &Path, inventory: AdminInventory) -> Result<Self> {
        let store = crate::store::SessionStore::open(root)?;
        let original = store.read()?;
        // Borrow paths from the one existing decode while loading its genuine
        // identity. Move the decoded input into the operation; never copy base.
        let signed: SignedAggregate = postcard::from_bytes(&original)?;
        schema::canonical(&signed, &original)?;
        let aggregate = schema::Aggregate::decode_administrative_payload(&signed.payload)?;
        let identity =
            crate::identity::load(&aggregate.identity_root, &aggregate.host_identity_root)?;
        let _authenticated = super::restore(&aggregate, &signed, &identity)?;
        let room = uuid::Uuid::parse_str(&aggregate.room)?;
        Ok(Self {
            output: None,
            physical_failure: None,
            physical: None,
            physical_credit: None,
            store,
            identity,
            original,
            signed,
            selection: aggregate.sender,
            room,
            generation: aggregate.generation,
            inventory,
        })
    }
    fn scope(&self) -> std::result::Result<SenderScope, Cause> {
        SenderScope::native(
            self.room,
            self.identity.native_cert.user_public_key,
            self.identity.native_cert.device_public_key,
        )
        .map_err(Cause::Code)
    }
    fn phase(&self) -> AdminPhase {
        if self.selection.expect("selected operation").0[1] == 0 {
            AdminPhase::Prepared
        } else {
            AdminPhase::Active
        }
    }
    fn status(&self) -> AdminStatus {
        let selected = self.selection.expect("selected operation");
        AdminStatus {
            phase: self.phase(),
            room: *self.room.as_bytes(),
            owner_root: self.identity.native_cert.user_public_key,
            native_device: self.identity.native_cert.device_public_key,
            instance: selected.0[83..99].try_into().expect("fixed instance"),
        }
    }
    fn execute(mut self, intent: Intent) -> std::result::Result<AdminStatus, AdminError> {
        match self.run(intent) {
            Ok(()) => Ok(self.status()),
            Err(cause) => Err(AdminError {
                cause,
                operation: Some(self),
            }),
        }
    }
    fn run(&mut self, intent: Intent) -> std::result::Result<(), Cause> {
        // Exact authenticated read bytes establish the prior acknowledged digest.
        // Do not canonicalize via ordinary encode before recording that identity.
        self.store.commit(&self.original)?;
        let bounded = BoundedOwnerDirectory::admit(self.store.administrative_directory()?)?;
        if intent == Intent::Select {
            if self.selection.is_some() {
                return Err(Cause::Refused(
                    "sender storage is already selected; use resume or status",
                ));
            }
            // Unexpected old artifacts do not constitute eligibility to reset.
            match bounded.open_read(OsStr::new("private-actions-v1.writer.lock")) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(Cause::Refused(
                        "unselected owner has existing sender artifacts",
                    ));
                }
                Err(e) => return Err(e.into()),
            }
            match bounded.open_directory_child(OsStr::new("private-actions-v1")) {
                Err(e) if e.io_error().kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(Cause::Refused(
                        "unselected owner has existing sender artifacts",
                    ));
                }
                Err(e) => return Err(e.into()),
            }
        } else if self.selection.is_none() {
            return Err(Cause::Refused(
                "sender storage is unselected; explicit select is required",
            ));
        }
        if intent == Intent::Status && self.phase() == AdminPhase::Prepared {
            return Ok(());
        }
        let scope = self.scope()?;
        let required = AdministrativeFile::required_bytes(&scope).map_err(Cause::Code)?;
        self.physical_credit = Some(self.inventory.allowance.reserve(required)?);
        if intent == Intent::Select {
            let mut selected = [0; 187];
            selected[0] = 1;
            selected[2] = 1;
            selected[3..19].copy_from_slice(self.room.as_bytes());
            selected[19..51].copy_from_slice(&self.identity.native_cert.user_public_key);
            selected[51..83].copy_from_slice(&self.identity.native_cert.device_public_key);
            selected[83..99].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
            selected[99..107].copy_from_slice(&1_u64.to_le_bytes());
            selected[115..147].copy_from_slice(blake3::hash(b"NSO1\x01\x00\x00\x00").as_bytes());
            selected[147..155].copy_from_slice(&self.generation.to_le_bytes());
            selected[155..187].copy_from_slice(&self.store.acknowledged_digest()?);
            self.commit_selection(schema::SenderSelection(selected))?;
        }
        let instance = self.status().instance;
        let expectation = match self.phase() {
            AdminPhase::Prepared => AdministrativeExpectation::Prepared,
            AdminPhase::Active => AdministrativeExpectation::Active,
        };
        let directory = self.store.administrative_directory()?;
        let credit = self.physical_credit.take().expect("prepaid physical work");
        #[cfg(test)]
        tests::boundary(Boundary::BeforePhysical, &self.store);
        let open = || AdministrativeFile::open(directory, scope, instance, expectation, credit);
        #[cfg(test)]
        let result = tests::physical(open);
        #[cfg(not(test))]
        let result = open();
        match result {
            Ok(file) => self.physical = Some(file),
            Err(error) => {
                self.physical_failure = Some(error);
                return Err(Cause::Physical);
            }
        }
        self.current()?;
        if self.phase() == AdminPhase::Prepared {
            let mut selected = self.selection.expect("prepared selection");
            selected.0[1] = 1;
            self.commit_selection(selected)?;
        }
        #[cfg(test)]
        tests::boundary(Boundary::AfterActive, &self.store);
        self.current()?;
        Ok(())
    }
    fn current(&mut self) -> std::result::Result<(), Cause> {
        self.store.administrative_directory()?;
        self.current_physical()
    }
    fn current_physical(&mut self) -> std::result::Result<(), Cause> {
        let file = self
            .physical
            .take()
            .ok_or(Cause::Refused("physical verification is absent"))?;
        match file.revalidate_owned() {
            Ok(file) => {
                self.physical = Some(file);
                Ok(())
            }
            Err(error) => {
                self.physical_failure = Some(error);
                Err(Cause::Physical)
            }
        }
    }
    fn commit_selection(
        &mut self,
        selected: schema::SenderSelection,
    ) -> std::result::Result<(), Cause> {
        if let Some(previous) = self.selection {
            let mut expected = previous;
            expected.0[1] = 1;
            if previous.0[1] != 0 || selected != expected {
                return Err(Cause::Refused("invalid administrative phase transition"));
            }
        } else if selected.0[1] != 0 {
            return Err(Cause::Refused("initial sender phase must be Prepared"));
        }
        let base = schema::administrative_base(&self.signed.payload)?;
        self.output = Some(encode_selected(
            base,
            &selected,
            &self.identity,
            &self.inventory.allowance,
        )?);
        let bytes = &self.output.as_ref().expect("funded output").signed;
        #[cfg(test)]
        tests::commit(selected.0[1], &mut self.store, bytes)?;
        #[cfg(not(test))]
        self.store.commit(bytes)?;
        self.selection = Some(selected);
        // The immutable original base is retained. A later phase never decodes
        // or copies this new wrapper, and never regenerates publication blobs.
        self.output = None;
        #[cfg(test)]
        if selected.0[1] == 0 {
            tests::boundary(Boundary::AfterPrepared, &self.store);
        }
        Ok(())
    }
}

fn encode_selected(
    base: &[u8],
    selected: &schema::SenderSelection,
    identity: &Identity,
    allowance: &NativeAllowance,
) -> std::result::Result<Encoded, Cause> {
    let wire = schema::AggregateV7 {
        version: 7,
        base,
        sender: &selected.0,
    };
    let payload_len = schema::counted_raw(&wire, crate::store::MAX_AGGREGATE_BYTES)?;
    let signed_len = schema::signed_size_raw(payload_len)
        .ok_or(Cause::Refused("selected output size overflow"))?;
    if signed_len > crate::store::MAX_AGGREGATE_BYTES {
        return Err(Cause::Refused(
            "selected aggregate exceeds total signed size limit",
        ));
    }
    // Original ordinary Store publication is not a paid-owner claim. Its
    // generated name backing is nevertheless retained with the new output.
    let name = BoundedOwnerDirectory::publication_allocation_bytes(OsStr::new("session.bin"))?;
    let total = payload_len
        .checked_add(signed_len)
        .and_then(|n| n.checked_add(name))
        .ok_or(Cause::Refused("selected output size overflow"))?;
    let credit = allowance.reserve(total)?;
    let payload = encode_exact(&wire, payload_len)?;
    let digest = blake3::Hasher::new_derive_key("qOS native session aggregate v1")
        .update(&payload)
        .finalize();
    let signature = identity.native.sign(digest.as_bytes()).to_bytes();
    let signed = encode_exact(
        &SignedView {
            payload: &payload,
            signature: &signature,
        },
        signed_len,
    )?;
    Ok(Encoded {
        _payload: payload,
        signed,
        _credit: credit,
    })
}
fn encode_exact<T: Serialize>(value: &T, length: usize) -> std::result::Result<Vec<u8>, Cause> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length)?;
    bytes.resize(length, 0);
    if bytes.capacity() != length || postcard::to_slice(value, &mut bytes)?.len() != length {
        return Err(Cause::Refused("selected output size changed"));
    }
    Ok(bytes)
}

pub fn initialize(root: &Path) -> std::result::Result<AdminStatus, AdminError> {
    let inventory = AdminInventory::new()?;
    let SessionOwner {
        store, identity, ..
    } = SessionOwner::initialize(root)?;
    Operation::admit(store, identity, inventory)?.execute(Intent::Select)
}
pub fn initialize_workspace(
    root: &Path,
    name: &str,
) -> std::result::Result<AdminStatus, AdminError> {
    let inventory = AdminInventory::new()?;
    let SessionOwner {
        store, identity, ..
    } = SessionOwner::initialize_workspace(root, name)?;
    Operation::admit(store, identity, inventory)?.execute(Intent::Select)
}
pub fn select(root: &Path) -> std::result::Result<AdminStatus, AdminError> {
    let inventory = AdminInventory::new()?;
    Operation::existing(root, inventory)?.execute(Intent::Select)
}
pub fn resume(root: &Path) -> std::result::Result<AdminStatus, AdminError> {
    let inventory = AdminInventory::new()?;
    Operation::existing(root, inventory)?.execute(Intent::Resume)
}
pub fn status(root: &Path) -> std::result::Result<AdminStatus, AdminError> {
    let inventory = AdminInventory::new()?;
    Operation::existing(root, inventory)?.execute(Intent::Status)
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boundary {
    AfterPrepared,
    BeforePhysical,
    AfterActive,
}
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests;
