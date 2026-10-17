//! Destination-staging declaration inside the authenticated setup package.
//! Deserialized bytes alone confer no source or caller authority.
use super::invalid;
use crate::setup::{
    home_capture::{MountedWitness, OriginalCallerFacts, SourceWitness},
    migration_handoff::{HomePolicy, Plan},
    portable_home_media::RegisteredOriginalSource,
};
use serde::{Deserialize, Serialize};
use std::io;
const LIMIT: usize = 64 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalCopyDeclaration {
    schema: u32,
    source: Plan,
    source_witness: SourceWitness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mounted_witness: Option<MountedWitness>,
    caller: OriginalCallerFacts,
    setup_binding: String,
    identity_public_key: [u8; 32],
    reservation: u64,
}
impl OriginalCopyDeclaration {
    pub fn new(
        registered: RegisteredOriginalSource,
        setup_binding: String,
        identity_public_key: [u8; 32],
        reservation: u64,
    ) -> io::Result<Self> {
        let value = Self {
            schema: 1,
            source: registered.plan,
            source_witness: registered.witness,
            mounted_witness: registered.mounted,
            caller: registered.original_caller,
            setup_binding,
            identity_public_key,
            reservation,
        };
        value.bytes()?;
        Ok(value)
    }
    pub fn validate(&self) -> io::Result<()> {
        self.source.validate()?;
        let operation = uuid::Uuid::parse_str(&self.source.operation_id).map_err(|_| invalid())?;
        if operation.is_nil() || operation.to_string() != self.source.operation_id {
            return Err(invalid());
        }
        if self.source.categories.flatpak
            && crate::setup::migration_handoff::OriginalInventoryBinding::parse(&self.source)?
                .is_none()
        {
            return Err(invalid());
        }
        self.source_witness.validate_plan(&self.source)?;
        self.caller.validate(self.source.home_uid())?;
        if let Some(mounted) = &self.mounted_witness {
            mounted.matches_source(&self.source_witness)?;
        }
        if self.schema != 1
            || self.source.home_policy != HomePolicy::Copy
            || !(self.source.categories.home || self.source.categories.flatpak)
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.setup_binding.chars().any(char::is_control)
            || self.identity_public_key == [0; 32]
            || self.reservation == 0
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn bytes(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let b = serde_json::to_vec(self)?;
        if b.len() > LIMIT {
            return Err(invalid());
        }
        Ok(b)
    }
    pub fn source(&self) -> &Plan {
        &self.source
    }
    pub fn source_witness(&self) -> &SourceWitness {
        &self.source_witness
    }
    pub fn mounted_witness(&self) -> Option<&MountedWitness> {
        self.mounted_witness.as_ref()
    }
    pub fn requested_caller(&self) -> &OriginalCallerFacts {
        &self.caller
    }
    pub fn setup_binding(&self) -> &str {
        &self.setup_binding
    }
    pub fn identity_public_key(&self) -> [u8; 32] {
        self.identity_public_key
    }
    pub fn reservation(&self) -> u64 {
        self.reservation
    }
}
