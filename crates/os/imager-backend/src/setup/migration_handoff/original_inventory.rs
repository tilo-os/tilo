//! Bounded marker for protected original inventory; never a path or access grant.
use super::*;
use sha2::{Digest, Sha256};
pub const INVENTORY_BOUND: u64 = 8 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalInventoryBinding {
    kind: String,
    schema: u32,
    operation: String,
    pub len: u64,
    pub sha256: [u8; 32],
}
impl OriginalInventoryBinding {
    pub fn parse(plan: &Plan) -> io::Result<Option<Self>> {
        let Some(raw) = &plan.flatpak_inventory else {
            return Ok(None);
        };
        if raw.get("kind").is_none() {
            return Ok(None);
        }
        let value: Self = serde_json::from_value(raw.clone())?;
        value.validate(&plan.operation_id)?;
        Ok(Some(value))
    }
    pub fn validate(&self, operation: &str) -> io::Result<()> {
        if self.kind != "protected-original-flatpak"
            || self.schema != 1
            || self.operation != operation
            || !operation_id(operation)
            || self.len == 0
            || self.len > INVENTORY_BOUND
            || self.sha256 == [0; 32]
        {
            return Err(refusal("Protected original inventory binding differs"));
        }
        Ok(())
    }
    pub fn validate_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        self.validate(&self.operation)?;
        if u64::try_from(bytes.len()).map_err(io::Error::other)? != self.len
            || <[u8; 32]>::from(Sha256::digest(bytes)) != self.sha256
        {
            return Err(refusal("Protected original inventory bytes differ"));
        }
        let inventory =
            super::super::flatpak_transfer::HandoffInventory::parse(bytes).map_err(refusal)?;
        inventory
            .validate_operation(&self.operation)
            .map_err(refusal)?;
        if inventory.bytes().map_err(refusal)? != bytes {
            return Err(refusal("Protected original inventory is not canonical"));
        }
        Ok(())
    }
}
impl Plan {
    /// Used only at original registration. Inline ordinary migration plans and
    /// their limits remain unchanged. Full public inventory is a private sidecar.
    pub fn original_inventory_selection(&self) -> io::Result<(Self, Option<Vec<u8>>)> {
        self.validate()?;
        if OriginalInventoryBinding::parse(self)?.is_some() {
            return Err(refusal(
                "Original inventory marker cannot supply registration bytes",
            ));
        }
        let mut compact = self.clone();
        let bytes = match &self.flatpak_inventory {
            Some(raw) => {
                let inventory = super::super::flatpak_transfer::HandoffInventory::parse(
                    &serde_json::to_vec(raw)?,
                )
                .map_err(refusal)?;
                inventory
                    .validate_operation(&self.operation_id)
                    .map_err(refusal)?;
                let bytes = inventory.bytes().map_err(refusal)?;
                let binding = OriginalInventoryBinding {
                    kind: "protected-original-flatpak".into(),
                    schema: 1,
                    operation: self.operation_id.clone(),
                    len: u64::try_from(bytes.len()).map_err(io::Error::other)?,
                    sha256: Sha256::digest(&bytes).into(),
                };
                binding.validate_bytes(&bytes)?;
                compact.flatpak_inventory = Some(serde_json::to_value(binding)?);
                Some(bytes)
            }
            None => None,
        };
        compact.bytes()?;
        Ok((compact, bytes))
    }
}
