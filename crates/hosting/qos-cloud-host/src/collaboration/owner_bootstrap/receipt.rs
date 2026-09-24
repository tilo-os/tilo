//! Finite, canonical typed receipt stored in the existing consumed map.
//! These bounds do not qualify the legacy Store's allocation/resource path.
use super::*;
use serde_json::Value;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_NODES: usize = 1_048_576;
const MAX_DEPTH: usize = 16;
const MAX_STRING: usize = 256 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReceiptPurpose {
    OwnerMembership,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnerReceipt {
    pub version: u32,
    pub purpose: ReceiptPurpose,
    pub proof_digest: [u8; 32],
    pub completed_at_ns: u64,
    pub issued: StoredMembershipChallenge,
    pub request: MembershipCompleteRequest,
    pub bootstrap: OwnerBootstrapV1,
}

struct LimitedWriter(usize);
impl std::io::Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn bounded_tree(value: &Value, depth: usize, nodes: &mut usize) -> Result<()> {
    if depth > MAX_DEPTH || *nodes >= MAX_NODES {
        bail!("owner receipt structural limit")
    }
    *nodes += 1;
    match value {
        Value::String(s) if s.len() > MAX_STRING => bail!("owner receipt string limit"),
        Value::Array(values) => {
            if values.len() > MAX_NODES - *nodes {
                bail!("owner receipt collection limit")
            }
            for value in values {
                bounded_tree(value, depth + 1, nodes)?;
            }
        }
        Value::Object(values) => {
            if values.len() > MAX_NODES - *nodes {
                bail!("owner receipt collection limit")
            }
            for (key, value) in values {
                if key.len() > MAX_STRING {
                    bail!("owner receipt key limit")
                }
                bounded_tree(value, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}
impl OwnerReceipt {
    fn validate(&self) -> Result<()> {
        if self.version != 1 || self.bootstrap.version != 1 {
            bail!("owner receipt version is not supported")
        }
        let value = &self.bootstrap;
        if value.room.len() > 64
            || value.certified_roster.len() > 65_536
            || value.revocations.len() > 65_536
            || value.records.len() > 65_536
            || value.governance_messages.len() > 4096
        {
            bail!("owner receipt bootstrap limit")
        }
        Ok(())
    }
    pub(super) fn encode(&self) -> Result<Value> {
        self.validate()?;
        serde_json::to_writer(LimitedWriter(0), self)?;
        let value = serde_json::to_value(self)?;
        bounded_tree(&value, 0, &mut 0)?;
        Ok(value)
    }
    pub(super) fn decode(value: &Value) -> Result<Self> {
        bounded_tree(value, 0, &mut 0)?;
        serde_json::to_writer(LimitedWriter(0), value)?;
        let receipt = Self::deserialize(value)?;
        receipt.validate()?;
        // Includes nested derived join/identity structures: no unknown ignored
        // field or alternative representation can be treated as a receipt.
        if receipt.encode()? != *value {
            bail!("owner receipt is not canonical")
        }
        Ok(receipt)
    }
}
