//! Content larger than one item: kept as parts, each an item of its own, and
//! a list naming them in order.
//!
//! An item travels and is kept whole (a frame, a worker's hand-over, a page's
//! store), so one item is bounded. Content of any size is kept a part at a
//! time instead ([`super::ContentStore::put_parts`]): each part is stored as
//! any item is (sealed, in a store that seals), and the list that names them
//! by their references is an item too, sealed the same way, so whoever does
//! not hold the list's key learns neither the parts' keys nor their order.
//! Reading the list's reference reads the content the parts make, as if it
//! were one item ([`super::ContentStore::open`], `read`, `size`, `read_range`).
//!
//! ```text
//! "TCL\x01" · parts u32 · total u64 · { len u64 · reference length u16 · reference }…
//! ```
//! all little endian, and nothing after the last part.

use std::io::Read;

pub(crate) const MAGIC: &[u8; 4] = b"TCL\x01";
const HEADER: usize = MAGIC.len() + 4 + 8;

/// The most bytes one part holds: far below what one item may be anywhere.
pub const PART_BYTES: usize = 8 * 1024 * 1024;

/// A list's parts: each one's length and reference, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct List {
    pub(crate) parts: Vec<(u64, String)>,
}

impl List {
    pub(crate) fn total(&self) -> u64 {
        self.parts
            .iter()
            .fold(0_u64, |total, (len, _)| total.saturating_add(*len))
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(
            &u32::try_from(self.parts.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&self.total().to_le_bytes());
        for (len, reference) in &self.parts {
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(
                &u16::try_from(reference.len())
                    .unwrap_or(u16::MAX)
                    .to_le_bytes(),
            );
            out.extend_from_slice(reference.as_bytes());
        }
        out
    }

    /// The list `reader` holds, read no further than a list is long; or what
    /// was read of it, when it holds no list, to read it again from the start.
    pub(crate) fn read(reader: &mut dyn Read) -> std::io::Result<Result<Self, Vec<u8>>> {
        let mut read = Vec::new();
        let header = take(reader, &mut read, HEADER)?;
        if header < HEADER || read.get(..MAGIC.len()) != Some(MAGIC.as_slice()) {
            return Ok(Err(read));
        }
        let count = u32_at(&read, MAGIC.len());
        let total = u64_at(&read, MAGIC.len().saturating_add(4));
        let mut parts = Vec::new();
        for _ in 0..count {
            let at = read.len();
            if take(reader, &mut read, 10)? < 10 {
                return Ok(Err(read));
            }
            let len = u64_at(&read, at);
            let named = usize::from(u16::from_le_bytes([
                read.get(at.saturating_add(8)).copied().unwrap_or(0),
                read.get(at.saturating_add(9)).copied().unwrap_or(0),
            ]));
            let from = read.len();
            if take(reader, &mut read, named)? < named {
                return Ok(Err(read));
            }
            let reference = std::str::from_utf8(read.get(from..).unwrap_or_default())
                .ok()
                .filter(|reference| super::valid_reference(reference))
                .map(str::to_owned);
            match reference {
                Some(reference) if len > 0 => parts.push((len, reference)),
                _ => return Ok(Err(read)),
            }
        }
        let list = Self { parts };
        // A list ends with its last part: bytes past it are a file that
        // merely begins like one.
        if list.parts.len() < 2 || list.total() != total || take(reader, &mut read, 1)? != 0 {
            return Ok(Err(read));
        }
        Ok(Ok(list))
    }
}

/// Read up to `n` more bytes of `reader` onto `into`, answering how many came.
fn take(reader: &mut dyn Read, into: &mut Vec<u8>, n: usize) -> std::io::Result<usize> {
    let before = into.len();
    reader.take(n as u64).read_to_end(into)?;
    Ok(into.len().saturating_sub(before))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut raw = [0_u8; 4];
    if let Some(found) = bytes.get(at..at.saturating_add(4)) {
        raw.copy_from_slice(found);
    }
    u32::from_le_bytes(raw)
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut raw = [0_u8; 8];
    if let Some(found) = bytes.get(at..at.saturating_add(8)) {
        raw.copy_from_slice(found);
    }
    u64::from_le_bytes(raw)
}

/// The content a list's parts make, read a part at a time: each part is
/// opened when the one before it ends.
pub(crate) struct Parts {
    pub(crate) store: super::ContentStore,
    /// The parts still to open, in reverse order.
    pub(crate) left: Vec<String>,
    pub(crate) current: Option<super::ContentReader>,
}

impl Read for Parts {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(current) = &mut self.current {
                let n = current.read(buf)?;
                if n > 0 || buf.is_empty() {
                    return Ok(n);
                }
                self.current = None;
            }
            let Some(next) = self.left.pop() else {
                return Ok(0);
            };
            self.current = Some(self.store.open_item(&next).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("a part of the content is not here: {next}"),
                )
            })?);
        }
    }
}
