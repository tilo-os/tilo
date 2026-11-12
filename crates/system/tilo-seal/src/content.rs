//! Content sealed under its own key: the bytes a record references (a
//! picture, an attachment), encrypted before they leave the writer's device,
//! so whoever stores or carries them holds ciphertext alone.
//!
//! Every item has a fresh random key, which travels in the reference the
//! record keeps, so the record's readers, and only they, open it. The content
//! is sealed in chunks of [`CHUNK`] bytes, each XChaCha20-Poly1305 under a
//! nonce made of its index and whether it is the last, so a reader opens and
//! checks each chunk as it streams in, and a stream cut short or reordered
//! fails to open. A key seals one item only, so a nonce derived from the
//! chunk's position is never used twice under it; sealing the same bytes
//! under the same key gives the same ciphertext.
//!
//! Layout: `"TCB" ‖ version (0x01) ‖ chunk₀ ‖ … ‖ chunkₙ`, each chunk its
//! sealed bytes and 16-byte tag. Every chunk but the last holds [`CHUNK`]
//! bytes; the last holds the rest, possibly none.

use std::io::{self, Read};

use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::error::{Error, Result};
use crate::keys::SymmetricKey;
use crate::label::Label;

/// The header every sealed item starts with.
pub const HEADER: &[u8; 4] = b"TCB\x01";
/// Plaintext bytes in each chunk but the last.
pub const CHUNK: usize = 64 * 1024;
/// Bytes sealing adds to each chunk.
pub const TAG: usize = 16;
const SEALED_CHUNK: usize = CHUNK + TAG;

fn cipher(key: &SymmetricKey) -> Result<XChaCha20Poly1305> {
    XChaCha20Poly1305::new_from_slice(key.expose_secret()).map_err(|_| Error::InvalidKey)
}

fn nonce(index: u64, last: bool) -> XNonce {
    let mut nonce = [0_u8; 24];
    let (at, rest) = nonce.split_at_mut(8);
    at.copy_from_slice(&index.to_le_bytes());
    if let Some(flag) = rest.first_mut() {
        *flag = u8::from(last);
    }
    XNonce::from(nonce)
}

const fn aad() -> &'static [u8] {
    Label::RecordContent.as_bytes()
}

/// How long `plain` bytes are once sealed.
#[must_use]
pub const fn sealed_len(plain: u64) -> u64 {
    let chunks = if plain == 0 {
        1
    } else {
        plain.div_ceil(CHUNK as u64)
    };
    (HEADER.len() as u64)
        .saturating_add(plain)
        .saturating_add(chunks.saturating_mul(TAG as u64))
}

/// Read `out.len()` bytes from `inner`, fewer only at its end.
fn fill(inner: &mut impl Read, out: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < out.len() {
        let Some(rest) = out.get_mut(filled..) else {
            break;
        };
        match inner.read(rest) {
            Ok(0) => break,
            Ok(n) => filled = filled.saturating_add(n),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// A chunked stream: the chunk at hand, and one byte read past it, which
/// says whether it is the last.
struct Chunks<R> {
    inner: R,
    size: usize,
    /// The byte read past the chunk at hand.
    ahead: Option<u8>,
    index: u64,
    out: Vec<u8>,
    at: usize,
    done: bool,
    started: bool,
}

impl<R: Read> Chunks<R> {
    fn new(inner: R, size: usize) -> Self {
        Self {
            inner,
            size,
            ahead: None,
            index: 0,
            out: Vec::new(),
            at: 0,
            done: false,
            started: false,
        }
    }

    /// The next chunk and whether it is the last; `None` past the last.
    fn next_chunk(&mut self) -> io::Result<Option<(Vec<u8>, bool)>> {
        if self.done {
            return Ok(None);
        }
        let mut chunk = vec![0_u8; self.size.saturating_add(1)];
        let mut filled: usize = 0;
        if let Some(byte) = self.ahead.take()
            && let Some(first) = chunk.first_mut()
        {
            *first = byte;
            filled = 1;
        }
        filled = filled.saturating_add(fill(
            &mut self.inner,
            chunk.get_mut(filled..).unwrap_or_default(),
        )?);
        let last = filled <= self.size;
        if !last {
            self.ahead = chunk.pop();
        }
        chunk.truncate(filled.min(self.size));
        self.done = last;
        Ok(Some((chunk, last)))
    }

    /// Copy what is ready to `buf`, making the next output with `make`.
    fn read_with(
        &mut self,
        buf: &mut [u8],
        mut make: impl FnMut(&mut Self) -> io::Result<Option<Vec<u8>>>,
    ) -> io::Result<usize> {
        while self.at >= self.out.len() {
            match make(self)? {
                Some(out) => {
                    self.out = out;
                    self.at = 0;
                }
                None => return Ok(0),
            }
        }
        let ready = self.out.get(self.at..).unwrap_or_default();
        let n = ready.len().min(buf.len());
        if let (Some(to), Some(from)) = (buf.get_mut(..n), ready.get(..n)) {
            to.copy_from_slice(from);
        }
        self.at = self.at.saturating_add(n);
        Ok(n)
    }
}

fn invalid(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// Seals what `inner` yields as it is read: a [`Read`] of the sealed item.
pub struct SealReader<R> {
    cipher: XChaCha20Poly1305,
    chunks: Chunks<R>,
}

impl<R: Read> SealReader<R> {
    /// Seal `inner` under `key`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidKey`] if the primitive refuses the key.
    pub fn new(key: &SymmetricKey, inner: R) -> Result<Self> {
        Ok(Self {
            cipher: cipher(key)?,
            chunks: Chunks::new(inner, CHUNK),
        })
    }
}

impl<R: Read> Read for SealReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let cipher = &self.cipher;
        self.chunks.read_with(buf, |chunks| {
            let header = !std::mem::replace(&mut chunks.started, true);
            let Some((plain, last)) = chunks.next_chunk()? else {
                return Ok(None);
            };
            let sealed = cipher
                .encrypt(
                    &nonce(chunks.index, last),
                    Payload {
                        msg: &plain,
                        aad: aad(),
                    },
                )
                .map_err(|_| invalid("content could not be sealed"))?;
            chunks.index = chunks.index.saturating_add(1);
            Ok(Some(if header {
                [HEADER.as_slice(), &sealed].concat()
            } else {
                sealed
            }))
        })
    }
}

/// Opens a sealed item as it is read: a [`Read`] of its plaintext that
/// fails with [`io::ErrorKind::InvalidData`] at the first chunk that does
/// not open, and when the item ends before its last chunk.
pub struct OpenReader<R> {
    cipher: XChaCha20Poly1305,
    chunks: Chunks<R>,
}

impl<R: Read> OpenReader<R> {
    /// Open `inner`, sealed under `key`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidKey`] if the primitive refuses the key.
    pub fn new(key: &SymmetricKey, inner: R) -> Result<Self> {
        Ok(Self {
            cipher: cipher(key)?,
            chunks: Chunks::new(inner, SEALED_CHUNK),
        })
    }
}

impl<R: Read> Read for OpenReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let cipher = &self.cipher;
        self.chunks.read_with(buf, |chunks| {
            if !std::mem::replace(&mut chunks.started, true) {
                let mut header = [0_u8; HEADER.len()];
                if fill(&mut chunks.inner, &mut header)? != HEADER.len() || &header != HEADER {
                    return Err(invalid("not sealed content"));
                }
            }
            let Some((sealed, last)) = chunks.next_chunk()? else {
                return Ok(None);
            };
            let plain = cipher
                .decrypt(
                    &nonce(chunks.index, last),
                    Payload {
                        msg: &sealed,
                        aad: aad(),
                    },
                )
                .map_err(|_| invalid("sealed content does not open"))?;
            chunks.index = chunks.index.saturating_add(1);
            Ok(Some(plain))
        })
    }
}

/// `plain` sealed under `key`.
///
/// # Errors
///
/// [`Error::InvalidKey`] or [`Error::Seal`].
pub fn seal(key: &SymmetricKey, plain: &[u8]) -> Result<Vec<u8>> {
    let mut sealed = Vec::new();
    SealReader::new(key, plain)?
        .read_to_end(&mut sealed)
        .map_err(|_| Error::Seal)?;
    Ok(sealed)
}

/// What [`seal`] sealed under `key`.
///
/// # Errors
///
/// [`Error::Authentication`] for another key, or bytes changed, cut short
/// or not sealed content.
pub fn open(key: &SymmetricKey, sealed: &[u8]) -> Result<Vec<u8>> {
    let mut plain = Vec::new();
    OpenReader::new(key, sealed)?
        .read_to_end(&mut plain)
        .map_err(|_| Error::Authentication)?;
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that hands out at most `step` bytes a call.
    struct Trickle<'a>(&'a [u8], usize);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.0.len().min(buf.len()).min(self.1);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn content_opens_under_its_key_whatever_its_length() {
        let key = SymmetricKey::generate().unwrap();
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 17] {
            let plain: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
            let sealed = seal(&key, &plain).unwrap();
            assert_eq!(sealed.len() as u64, sealed_len(len as u64), "{len}");
            assert_eq!(
                sealed,
                seal(&key, &plain).unwrap(),
                "one key, one ciphertext"
            );
            assert_eq!(open(&key, &sealed).unwrap(), plain, "{len}");
            let mut streamed = Vec::new();
            OpenReader::new(&key, Trickle(&sealed, 7))
                .unwrap()
                .read_to_end(&mut streamed)
                .unwrap();
            assert_eq!(streamed, plain, "{len}, read in small steps");
            let mut resealed = Vec::new();
            SealReader::new(&key, Trickle(&plain, 5))
                .unwrap()
                .read_to_end(&mut resealed)
                .unwrap();
            assert_eq!(resealed, sealed, "{len}, sealed in small steps");
            // Where the window is the whole plaintext, a short one is found by chance: one
            // byte of a chunk matches it about once in every 256 positions. Only a plaintext
            // at least as long as the window is evidence of the ciphertext not carrying it.
            if len >= 32 {
                assert!(
                    !sealed
                        .windows(len.min(32))
                        .any(|w| w == &plain[..len.min(32)]),
                    "no plaintext in the sealed bytes"
                );
            }
        }
    }

    #[test]
    fn content_changed_cut_or_under_another_key_does_not_open() {
        let key = SymmetricKey::generate().unwrap();
        let plain = vec![5_u8; 2 * CHUNK + 3];
        let sealed = seal(&key, &plain).unwrap();
        let other = SymmetricKey::generate().unwrap();
        assert_eq!(open(&other, &sealed), Err(Error::Authentication));
        let mut changed = sealed.clone();
        changed[HEADER.len() + CHUNK + 3] ^= 1;
        assert_eq!(open(&key, &changed), Err(Error::Authentication));
        let cut = &sealed[..HEADER.len() + 2 * SEALED_CHUNK];
        assert_eq!(
            open(&key, cut),
            Err(Error::Authentication),
            "last chunk gone"
        );
        assert_eq!(open(&key, &sealed[..2]), Err(Error::Authentication));
        assert_eq!(open(&key, &plain), Err(Error::Authentication));
    }
}
