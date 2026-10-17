//! Bounded framing around the existing reviewed tilo-seal construction.
use super::Purpose;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read},
    sync::atomic::{AtomicBool, Ordering},
};
use tilo_seal::{
    Context, Label, PasswordParams, SymmetricKey,
    envelope::{self, Envelope, KeySource},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    purpose: Purpose,
    key: [u8; 32],
}

impl Drop for Header {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

const MAGIC: &[u8; 8] = b"TILOMIG1";
pub(super) const CHUNK: usize = 1024 * 1024;
const MAX_ENVELOPE: usize = CHUNK + 256;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn crypto(error: tilo_seal::Error) -> io::Error {
    let message = format!("Migration package authentication failed: {error}");
    io::Error::new(
        io::ErrorKind::InvalidData,
        AuthenticationError { message, error },
    )
}

#[derive(Debug)]
struct AuthenticationError {
    message: String,
    error: tilo_seal::Error,
}
impl std::fmt::Display for AuthenticationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for AuthenticationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
fn context(prefix: &[u8], index: u64, footer: bool) -> Vec<u8> {
    let mut record = prefix.to_vec();
    record.extend_from_slice(&index.to_le_bytes());
    record.push(u8::from(footer));
    record
}
fn read_frame(reader: &mut impl Read, max: usize) -> io::Result<Vec<u8>> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > max {
        return Err(invalid("Migration frame exceeds bounds"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Footer {
    pub chunks: u64,
    pub bytes: u64,
    pub sha256: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_bytes: Option<u64>,
    #[serde(default)]
    pub runtime_sockets: u64,
    #[serde(default)]
    pub setup_binding: Option<String>,
}

pub(super) struct Opener<'a, R: Read> {
    reader: R,
    prefix: Vec<u8>,
    key: SymmetricKey,
    plaintext: Zeroizing<Vec<u8>>,
    position: usize,
    digest: Sha256,
    chunks: u64,
    bytes: u64,
    maximum: u64,
    cancel: &'a AtomicBool,
    pub footer: Option<Footer>,
    pub purpose: Purpose,
}
impl<'a, R: Read> Opener<'a, R> {
    pub fn new(
        mut reader: R,
        password: &[u8],
        maximum: u64,
        cancel: &'a AtomicBool,
    ) -> io::Result<Self> {
        let mut prefix = vec![0; 40];
        reader.read_exact(&mut prefix)?;
        if prefix.get(..8) != Some(MAGIC.as_slice()) {
            return Err(invalid("Unknown migration package"));
        }
        let sealed = read_frame(&mut reader, 1024)?;
        match Envelope::parse(&sealed).map_err(crypto)?.source {
            KeySource::Password { params, .. } if params == PasswordParams::DEFAULT => {}
            _ => return Err(invalid("Unsupported migration password parameters")),
        }
        let key = envelope::open_with_password(
            password,
            Context::new(Label::MigrationPackage, &prefix),
            &sealed,
        )
        .map_err(crypto)?;
        let header: Header = serde_json::from_slice(&key)?;
        let key = SymmetricKey::from_slice(&header.key).map_err(crypto)?;
        Ok(Self {
            reader,
            prefix,
            key,
            plaintext: Zeroizing::new(Vec::new()),
            position: 0,
            digest: Sha256::new(),
            chunks: 0,
            bytes: 0,
            maximum,
            cancel,
            footer: None,
            purpose: header.purpose,
        })
    }
    fn next(&mut self) -> io::Result<()> {
        let frame = read_frame(&mut self.reader, MAX_ENVELOPE)?;
        if frame.is_empty() {
            let record = context(&self.prefix, self.chunks, true);
            let sealed = read_frame(&mut self.reader, 1024)?;
            let bytes = envelope::open(
                &self.key,
                Context::new(Label::MigrationPackage, &record),
                &sealed,
            )
            .map_err(crypto)?;
            let footer: Footer = serde_json::from_slice(&bytes)?;
            if footer.bytes != self.bytes
                || footer.chunks != self.chunks
                || footer.sha256 != <[u8; 32]>::from(self.digest.clone().finalize())
            {
                return Err(invalid("Migration package is incomplete or changed"));
            }
            let mut extra = [0];
            if self.reader.read(&mut extra)? != 0 {
                return Err(invalid("Unexpected data after migration package"));
            }
            self.footer = Some(footer);
            self.plaintext.clear();
            self.position = 0;
            return Ok(());
        }
        let record = context(&self.prefix, self.chunks, false);
        self.plaintext = envelope::open(
            &self.key,
            Context::new(Label::MigrationPackage, &record),
            &frame,
        )
        .map_err(crypto)?;
        if self.plaintext.is_empty() || self.plaintext.len() > CHUNK {
            return Err(invalid("Invalid migration chunk"));
        }
        self.bytes = self
            .bytes
            .checked_add(self.plaintext.len() as u64)
            .filter(|size| *size <= self.maximum)
            .ok_or_else(|| invalid("Migration payload exceeds reviewed size limit"))?;
        self.digest.update(&self.plaintext);
        self.chunks = self
            .chunks
            .checked_add(1)
            .ok_or_else(|| invalid("Chunk count exceeds bounds"))?;
        self.position = 0;
        Ok(())
    }
}
impl<R: Read> Read for Opener<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.cancel.load(Ordering::Relaxed) {
            // read_exact/io::copy retry Interrupted, so cancellation is terminal.
            return Err(io::Error::other("Migration import cancelled"));
        }
        if self.footer.is_some() {
            return Ok(0);
        }
        if self.position == self.plaintext.len() {
            self.next()?;
        }
        let length = out
            .len()
            .min(self.plaintext.len().saturating_sub(self.position));
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| invalid("Prepared plaintext offset overflows"))?;
        let destination = out
            .get_mut(..length)
            .ok_or_else(|| invalid("Prepared plaintext output is too short"))?;
        let source = self
            .plaintext
            .get(self.position..end)
            .ok_or_else(|| invalid("Prepared plaintext offset is invalid"))?;
        destination.copy_from_slice(source);
        self.position = end;
        Ok(length)
    }
}
