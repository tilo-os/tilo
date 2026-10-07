//! Native Windows SHA-256 for verified packaged-font loading.
use std::io::{self, Read};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_SHA256_ALGORITHM, BCryptCloseAlgorithmProvider,
    BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptHashData,
    BCryptOpenAlgorithmProvider,
};

fn check(status: i32, operation: &str) -> io::Result<()> {
    if status >= 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{operation} failed: NTSTATUS {status:#x}"
        )))
    }
}

struct Sha256 {
    algorithm: BCRYPT_ALG_HANDLE,
    hash: BCRYPT_HASH_HANDLE,
}

impl Sha256 {
    fn new() -> io::Result<Self> {
        let mut state = Self {
            algorithm: null_mut(),
            hash: null_mut(),
        };
        // SAFETY: writable handle storage, static NUL-terminated algorithm
        // name and no optional provider. Drop closes any acquired handles.
        check(
            unsafe {
                BCryptOpenAlgorithmProvider(
                    &raw mut state.algorithm,
                    BCRYPT_SHA256_ALGORITHM,
                    null(),
                    0,
                )
            },
            "BCryptOpenAlgorithmProvider",
        )?;
        // SAFETY: live SHA-256 provider and writable output handle. On supported
        // Windows versions, a null object buffer requests provider allocation;
        // BCryptDestroyHash releases it. No keyed-hash secret is supplied.
        check(
            unsafe {
                BCryptCreateHash(
                    state.algorithm,
                    &raw mut state.hash,
                    null_mut(),
                    0,
                    null(),
                    0,
                    0,
                )
            },
            "BCryptCreateHash",
        )?;
        Ok(state)
    }

    fn update(&mut self, bytes: &[u8]) -> io::Result<()> {
        let length = u32::try_from(bytes.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "hash input chunk too large")
        })?;
        // SAFETY: the live hash and the input slice remain valid during this
        // synchronous call; length was checked against the Windows ABI.
        check(
            unsafe { BCryptHashData(self.hash, bytes.as_ptr(), length, 0) },
            "BCryptHashData",
        )
    }

    fn finish(self) -> io::Result<[u8; 32]> {
        let mut digest = [0; 32];
        // SAFETY: SHA-256 always produces 32 bytes; the writable output buffer
        // is exactly that length. Consuming self prevents reuse after finish.
        check(
            unsafe { BCryptFinishHash(self.hash, digest.as_mut_ptr(), 32, 0) },
            "BCryptFinishHash",
        )?;
        Ok(digest)
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        // SAFETY: these owned handles are closed once, with the hash destroyed
        // before its provider. Both may be null after partial initialization.
        unsafe {
            if !self.hash.is_null() {
                BCryptDestroyHash(self.hash);
            }
            if !self.algorithm.is_null() {
                BCryptCloseAlgorithmProvider(self.algorithm, 0);
            }
        }
    }
}

#[allow(
    clippy::large_stack_arrays,
    reason = "The fixed 64 KiB streaming hash buffer bounds memory without a heap allocation"
)]
pub(super) fn digest(mut reader: impl Read) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new()?;
    let mut buffer = [0; 64 * 1024];
    loop {
        let length = match reader.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if length == 0 {
            return hash.finish();
        }
        let bytes = buffer.get(..length).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "reader exceeded hash buffer")
        })?;
        hash.update(bytes)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    #[test]
    fn packaged_font_digests_are_complete_sha256_over_every_input() {
        // phase: same_sha256_for_empty_short_and_multichunk_inputs
        {
            for bytes in [vec![], b"abc".to_vec(), vec![0x61; 1_000_000]] {
                assert_eq!(
                    digest(bytes.as_slice()).unwrap().as_slice(),
                    sha2::Sha256::digest(&bytes).as_slice()
                );
            }
        }
        // phase: read_failure_is_not_a_successful_partial_digest
        {
            struct Failure;
            impl Read for Failure {
                fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "fixture denial",
                    ))
                }
            }
            assert_eq!(
                digest(Failure).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        // phase: packaged_product_fonts_match_both_manifest_and_rust_sha256
        {
            let root = crate::font_catalog::checkout_root();
            let manifest = std::fs::read_to_string(root.join("manifest.toml")).unwrap();
            let mut checked = 0;
            for asset in manifest.split("[[font]]").skip(1) {
                if !asset.lines().any(|line| line == "scope = \"product\"") {
                    continue;
                }
                let value = |name: &str| {
                    let prefix = format!("{name} = \"");
                    asset
                        .lines()
                        .find_map(|line| line.strip_prefix(&prefix)?.strip_suffix('"'))
                        .unwrap()
                };
                let path = root.join(value("file"));
                let bytes = std::fs::read(&path).unwrap();
                let actual = digest(std::fs::File::open(&path).unwrap()).unwrap();
                assert_eq!(
                    actual.as_slice(),
                    sha2::Sha256::digest(&bytes).as_slice(),
                    "{path:?}"
                );
                let hex = hex::encode(actual);
                assert_eq!(hex, value("sha256"), "{path:?}");
                checked += 1;
            }
            assert!(checked > 0, "must exercise actual product font files");
            println!("Verified {checked} product font digests");
        }
    }
}
