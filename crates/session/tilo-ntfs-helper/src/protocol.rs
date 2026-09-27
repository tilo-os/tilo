//! Diagnostic facts. None of these serializable values are capabilities to open or mutate a volume.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io;
#[derive(Default, Debug, Serialize, Deserialize)]
#[repr(C)]
pub struct Stats {
    pub bytes: u64,
    pub calls: u64,
    pub writes: u64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    pub source_pin: String,
    pub diagnostic_only: bool,
    pub memory_limit_enforced: bool,
    pub metadata_complete: bool,
    pub native_identity: Option<NativeIdentity>,
    pub status: i32,
    pub facts: Vec<serde_json::Value>,
    pub stats: Stats,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct NativeIdentity {
    pub volume: String,
    pub root: String,
    pub serial: String,
    pub root_record: u64,
    pub root_sequence: u64,
    pub root_creation: u64,
    pub upcase_sha256: String,
}
fn invalid() -> io::Error {
    io::Error::other("native identity facts incomplete or invalid")
}
fn hash(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    h.update(domain);
    for part in parts {
        h.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        h.update(part);
    }
    hex::encode(h.finalize())
}
fn unhex(text: &str) -> io::Result<Vec<u8>> {
    hex::decode(text).map_err(|_| invalid())
}
/// Hashes complete native facts using domain separation and length encoding.
/// Hardware/partition binding is performed separately by the retained Linux claim owner.
///
/// # Errors
/// Rejects missing, duplicate, partial or invalid native fields.
pub fn identity(report: &Report, root_record: u64) -> io::Result<NativeIdentity> {
    if report.status != 0 {
        return Err(invalid());
    }
    let mut volumes = report
        .facts
        .iter()
        .filter(|v| v.get("kind").and_then(serde_json::Value::as_str) == Some("volume"));
    let volume = volumes.next().ok_or_else(invalid)?;
    if volumes.next().is_some() {
        return Err(invalid());
    }
    let serial = volume
        .get("serial")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(invalid)?;
    if serial.len() != 16 || !serial.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let geometry = ["sector_size", "cluster_size", "mft_record_size"]
        .map(|key| volume.get(key).and_then(serde_json::Value::as_u64));
    let [Some(sector), Some(cluster), Some(mft)] = geometry else {
        return Err(invalid());
    };
    let volume_digest = hash(
        b"tilo-ntfs-native-volume-v1\0",
        &[
            serial.as_bytes(),
            &sector.to_le_bytes(),
            &cluster.to_le_bytes(),
            &mft.to_le_bytes(),
        ],
    );
    let mut roots = report.facts.iter().filter(|v| {
        v.get("kind").and_then(serde_json::Value::as_str) == Some("entry")
            && v.get("record").and_then(serde_json::Value::as_u64) == Some(root_record)
    });
    let root = roots.next().ok_or_else(invalid)?;
    if root.get("directory").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(invalid());
    }
    if roots.next().is_some() {
        return Err(invalid());
    }
    let sequence = root
        .get("sequence")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(invalid)?;
    if sequence == 0 || sequence > 65_535 {
        return Err(invalid());
    }
    let (creation, upcase_sha256) = native_metadata(report, root_record)?;
    let root_digest = hash(
        b"tilo-ntfs-native-root-v1\0",
        &[
            volume_digest.as_bytes(),
            &root_record.to_le_bytes(),
            &sequence.to_le_bytes(),
            &creation.to_le_bytes(),
        ],
    );
    Ok(NativeIdentity {
        volume: volume_digest,
        root: root_digest,
        serial: serial.to_owned(),
        root_record,
        root_sequence: sequence,
        root_creation: creation,
        upcase_sha256,
    })
}

fn native_metadata(report: &Report, root_record: u64) -> io::Result<(u64, String)> {
    let mut upcase = Vec::with_capacity(131_072);
    let mut creation = None;
    for fact in &report.facts {
        if fact.get("kind").and_then(serde_json::Value::as_str) != Some("chunk") {
            continue;
        }
        let record = fact.get("record").and_then(serde_json::Value::as_u64);
        let attribute = fact.get("attribute").and_then(serde_json::Value::as_u64);
        let offset = fact.get("offset").and_then(serde_json::Value::as_u64);
        if record == Some(10)
            && attribute == Some(128)
            && fact.get("name").and_then(serde_json::Value::as_str) == Some("")
        {
            if offset != Some(u64::try_from(upcase.len()).map_err(io::Error::other)?) {
                return Err(invalid());
            }
            let data = unhex(
                fact.get("hex")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(invalid)?,
            )?;
            if upcase.len().saturating_add(data.len()) > 131_072 {
                return Err(invalid());
            }
            upcase.extend_from_slice(&data);
        }
        if record == Some(root_record) && attribute == Some(16) && offset == Some(0) {
            if creation.is_some() {
                return Err(invalid());
            }
            let bytes = unhex(
                fact.get("hex")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(invalid)?,
            )?;
            creation = Some(u64::from_le_bytes(
                bytes
                    .get(..8)
                    .ok_or_else(invalid)?
                    .try_into()
                    .map_err(io::Error::other)?,
            ));
        }
    }
    if upcase.len() != 131_072 {
        return Err(invalid());
    }
    let creation = creation.ok_or_else(invalid)?;
    Ok((creation, hex::encode(Sha256::digest(&upcase))))
}
