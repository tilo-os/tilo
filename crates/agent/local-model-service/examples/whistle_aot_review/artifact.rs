//! Exact private factory output, not caller-selected native-code authority.
//! Pinned Wasmtime 49.0.2 / x86-64-v3 / default signal and Spectre protections.
use super::custody;
use local_model_service::speech::SpeechError;
use zeroize::Zeroizing;

const SHA: &str = "a74daea810fcccb4053ff8639f7bae9b7010fd9d9e1ad80a0deecc17c2cb3d26";
const SIZE: usize = 3_437_656;

pub fn verified(path: &str, digest: &str) -> Result<Zeroizing<Vec<u8>>, SpeechError> {
    if digest != SHA {
        return Err(SpeechError::ModelReceipt);
    }
    let bytes = custody::verified(path, SIZE, SHA)?;
    if bytes.len() != SIZE {
        return Err(SpeechError::ModelReceipt);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn caller_cannot_authorize_own_compiled_bytes_with_matching_hash() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("untrusted.aot");
        let bytes = b"caller selected native code";
        std::fs::write(&path, bytes).unwrap();
        let caller_digest = hex::encode(Sha256::digest(bytes));
        assert!(verified(path.to_str().unwrap(), &caller_digest).is_err());
        assert!(verified(path.to_str().unwrap(), SHA).is_err());
    }
}
