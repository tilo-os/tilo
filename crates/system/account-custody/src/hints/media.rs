//! Password-authenticated Windows suggestions in the existing 64 KiB media tail.
//!
//! This is not a Linux identity package. Enrollment and every sign-in happen
//! in the live session. The public expectation contains hashes, never hints.
use super::{AccountHint, PasswordHints};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Seek, SeekFrom};

pub const BLOCK_BYTES: usize = 65536;
pub const PRIVATE_OFFSET: usize = 32768;
const MAGIC: &[u8; 8] = b"TILOWIN1";
const MAX_CIPHER: usize = BLOCK_BYTES - PRIVATE_OFFSET - 12;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Identity {
    NewUserInLiveSession,
}

/// Descriptive binding only: it never authorizes replacement or enrollment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "RawExpectation")]
pub struct Expectation {
    pub schema: u32,
    pub identity: Identity,
    pub binding: String,
    pub sealed_bytes: u32,
    pub ciphertext_sha256: String,
    pub recovery_receipt_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExpectation {
    schema: u32,
    identity: Identity,
    binding: String,
    sealed_bytes: u32,
    ciphertext_sha256: String,
    recovery_receipt_sha256: String,
}
impl TryFrom<RawExpectation> for Expectation {
    type Error = &'static str;
    fn try_from(raw: RawExpectation) -> Result<Self, Self::Error> {
        let value = Self {
            schema: raw.schema,
            identity: raw.identity,
            binding: raw.binding,
            sealed_bytes: raw.sealed_bytes,
            ciphertext_sha256: raw.ciphertext_sha256,
            recovery_receipt_sha256: raw.recovery_receipt_sha256,
        };
        if value.valid() {
            Ok(value)
        } else {
            Err("invalid Windows media expectation")
        }
    }
}
fn hash(bytes: &[u8]) -> String {
    use std::fmt::Write;

    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            // Formatting into a String is infallible.
            let _ = write!(output, "{byte:02x}");
            output
        })
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Expectation {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && self.binding.len() == 36
            && self.binding.bytes().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
                }
            })
            && (48..=MAX_CIPHER).contains(&(self.sealed_bytes as usize))
            && digest(&self.ciphertext_sha256)
            && digest(&self.recovery_receipt_sha256)
    }
    fn context(&self) -> String {
        format!(
            "windows-new-user/v1/{}/{}",
            self.binding, self.recovery_receipt_sha256
        )
    }
}

/// Held only in the writer's private request, never collaborative form state.
/// Deserialization is not admission: `embed` independently verifies every byte.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepared {
    pub expectation: Expectation,
    ciphertext: Vec<u8>,
}
impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedWindowsSuggestions")
            .field("expectation", &self.expectation)
            .finish_non_exhaustive()
    }
}
fn invalid() -> io::Error {
    io::Error::other("Windows preparation differs from reviewed media expectation")
}
impl Prepared {
    /// Seal only after independent recovery copies and their receipt are durable.
    /// The receipt is authenticated as associated data, not merely checksummed.
    ///
    /// # Errors
    /// Rejects invalid bindings, passwords or hints, sealing failures, and
    /// ciphertext that exceeds the reserved media capacity.
    pub fn seal(
        password: &[u8],
        binding: &str,
        hints: &[AccountHint],
        receipt: &[u8],
    ) -> io::Result<Self> {
        let mut expectation = Expectation {
            schema: 1,
            identity: Identity::NewUserInLiveSession,
            binding: binding.into(),
            sealed_bytes: 48,
            ciphertext_sha256: "0".repeat(64),
            recovery_receipt_sha256: hash(receipt),
        };
        if !expectation.valid() {
            return Err(invalid());
        }
        let sealed = super::seal_password(password, &expectation.context(), hints)
            .map_err(io::Error::other)?;
        expectation.sealed_bytes = u32::try_from(sealed.as_bytes().len()).map_err(|_| invalid())?;
        expectation.ciphertext_sha256 = hash(sealed.as_bytes());
        let prepared = Self {
            expectation,
            ciphertext: sealed.as_bytes().to_vec(),
        };
        prepared.verify()?;
        Ok(prepared)
    }
    /// Validate the expectation, ciphertext digest, size and password envelope.
    ///
    /// # Errors
    /// Rejects malformed expectations, mismatched ciphertext or invalid envelopes.
    pub fn verify(&self) -> io::Result<()> {
        if !self.expectation.valid()
            || self.ciphertext.len() != self.expectation.sealed_bytes as usize
            || hash(&self.ciphertext) != self.expectation.ciphertext_sha256
        {
            return Err(invalid());
        }
        PasswordHints::from_bytes(&self.ciphertext).map_err(io::Error::other)?;
        Ok(())
    }
    /// Check that this preparation authenticates the supplied recovery receipt.
    ///
    /// # Errors
    /// Rejects invalid preparation or a receipt whose digest differs.
    pub fn verify_receipt(&self, receipt: &[u8]) -> io::Result<()> {
        self.verify()?;
        if hash(receipt) != self.expectation.recovery_receipt_sha256 {
            return Err(invalid());
        }
        Ok(())
    }
    /// Add ciphertext only to reserved zero padding; never overwrite profile JSON.
    ///
    /// # Errors
    /// Rejects invalid preparation, incorrectly sized blocks or nonzero padding.
    pub fn embed(&self, block: &mut [u8]) -> io::Result<()> {
        self.verify()?;
        if block.len() != BLOCK_BYTES {
            return Err(invalid());
        }
        let slot = block.get_mut(PRIVATE_OFFSET..).ok_or_else(invalid)?;
        if slot.iter().any(|b| *b != 0) {
            return Err(invalid());
        }
        let (header, payload) = slot.split_at_mut(12);
        let (magic, length) = header.split_at_mut(8);
        let destination = payload
            .get_mut(..self.ciphertext.len())
            .ok_or_else(invalid)?;
        magic.copy_from_slice(MAGIC);
        length.copy_from_slice(&self.expectation.sealed_bytes.to_be_bytes());
        destination.copy_from_slice(&self.ciphertext);
        Ok(())
    }
}

/// Reads from the same already-open medium that supplied the profile. No path
/// lookup, root-owned marker or guessed attachment can stand in for that source.
/// The caller must bind this expectation to its current private operation.
///
/// # Errors
/// Reports seek/read failures and rejects invalid expectations, mismatched
/// headers, ciphertext digests or password envelopes.
pub fn read(
    source: &mut (impl Read + Seek),
    size: u64,
    expected: &Expectation,
) -> io::Result<Prepared> {
    if !expected.valid() {
        return Err(invalid());
    }
    let offset = size
        .checked_sub((BLOCK_BYTES - PRIVATE_OFFSET) as u64)
        .ok_or_else(invalid)?;
    source.seek(SeekFrom::Start(offset))?;
    let mut header = [0; 12];
    source.read_exact(&mut header)?;
    if &header[..8] != MAGIC || header[8..12] != expected.sealed_bytes.to_be_bytes() {
        return Err(invalid());
    }
    let mut ciphertext = vec![0; expected.sealed_bytes as usize];
    source.read_exact(&mut ciphertext)?;
    let prepared = Prepared {
        expectation: expected.clone(),
        ciphertext,
    };
    prepared.verify()?;
    Ok(prepared)
}

impl Prepared {
    /// Explicit password authentication, with the current operation's exact
    /// expectation checked again before any plaintext leaves private custody.
    ///
    /// # Errors
    /// Rejects mismatched expectations, invalid preparation, failed password
    /// authentication or malformed decrypted hints.
    pub fn unlock(&self, password: &[u8], expected: &Expectation) -> io::Result<Vec<AccountHint>> {
        if &self.expectation != expected {
            return Err(invalid());
        }
        self.verify()?;
        let sealed = PasswordHints::from_bytes(&self.ciphertext).map_err(io::Error::other)?;
        super::open_password(password, &expected.context(), &sealed).map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn production_media_roundtrip_is_private_authenticated_and_receipt_bound() {
        let hint = AccountHint {
            provider: super::super::Provider::Microsoft,
            email: "private-person@example.org".into(),
            server: String::new(),
            sources: super::super::HintSources::default(),
        };
        let prepared = Prepared::seal(
            b"private-password",
            "12345678-1234-1234-1234-123456789abc",
            &[hint],
            b"durable independent copies",
        )
        .unwrap();
        let expected = prepared.expectation.clone();
        let mut block = vec![0; BLOCK_BYTES];
        prepared.embed(&mut block).unwrap();
        let public = serde_json::to_string(&expected).unwrap();
        assert!(!public.contains("private-person"));
        assert!(!block.windows(14).any(|b| b == b"private-person"));
        let mut source = io::Cursor::new(block);
        let read_back = read(&mut source, BLOCK_BYTES as u64, &expected).unwrap();
        assert!(read_back.unlock(b"wrong-password", &expected).is_err());
        assert_eq!(
            read_back.unlock(b"private-password", &expected).unwrap()[0].email,
            "private-person@example.org"
        );
        let mut other = expected.clone();
        other.recovery_receipt_sha256 = "1".repeat(64);
        assert!(read_back.unlock(b"private-password", &other).is_err());
        assert!(prepared.verify_receipt(b"different copies").is_err());
        source.get_mut()[PRIVATE_OFFSET + 20] ^= 1;
        assert!(read(&mut source, BLOCK_BYTES as u64, &expected).is_err());
        assert!(
            serde_json::from_str::<Expectation>(
                &public.replace("new_user_in_live_session", "root_identity")
            )
            .is_err()
        );
    }
}
