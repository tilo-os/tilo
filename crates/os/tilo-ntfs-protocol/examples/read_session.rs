//! Opt-in client exercise: connect to an already admitted administrative session.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use sha2::{Digest, Sha256};
    use std::{collections::BTreeMap, path::Path};
    use tilo_ntfs_protocol::{CHUNK_BYTES, HASH_BYTES, Identity, client::Client};
    let locator: serde_json::Value =
        serde_json::from_str(&std::env::var("TILO_NTFS_SESSION_LOCATOR")?)?;
    let path = locator
        .get("socket")
        .and_then(serde_json::Value::as_str)
        .ok_or("socket")?;
    let nonce = locator
        .get("nonce")
        .and_then(serde_json::Value::as_str)
        .ok_or("nonce")?
        .to_owned();
    let identity: Identity =
        serde_json::from_value(locator.get("identity").ok_or("identity")?.clone())?;
    let mut client = Client::connect(Path::new(path), nonce, identity)?;
    let manifest = client.begin()?;
    let mut hashes = BTreeMap::new();
    let mut total = 0u64;
    let mut large = false;
    for entry in &manifest.entries {
        if !entry.content_readable || total.saturating_add(entry.size) > HASH_BYTES {
            continue;
        }
        let mut offset = 0u64;
        let mut hash = Sha256::new();
        while offset < entry.size {
            let length = u32::try_from(
                entry
                    .size
                    .saturating_sub(offset)
                    .min(u64::from(CHUNK_BYTES)),
            )?;
            hash.update(client.read_chunk(&entry.token, offset, length)?);
            offset = offset.checked_add(u64::from(length)).ok_or("overflow")?;
        }
        let digest = hex::encode(hash.finalize());
        if entry.size == 12 * 1024 * 1024 {
            let expected = std::env::var("TILO_NTFS_NATIVE_FILE_SHA256")?;
            if digest != expected {
                return Err("independent 12MiB hash mismatch".into());
            }
            large = true;
        }
        hashes.insert(entry.token.clone(), digest);
        total = total.checked_add(entry.size).ok_or("budget")?;
    }
    let (attested, pending) = client.finish()?;
    if hashes != attested || !large {
        return Err("postflight or real large fixture missing".into());
    }
    client.cancel();
    println!(
        "{}",
        serde_json::json!({"native_bytes":total,"large_file_verified":true,"files":hashes.len(),"pending":pending.len(),"public_safe_read":false})
    );
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() -> Result<(), &'static str> {
    Err("administrative read sessions require Linux")
}
