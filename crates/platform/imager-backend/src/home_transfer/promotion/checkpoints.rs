//! Derived checkpoint membership only. Every bit is rebuilt from the held v1
//! journal; this anonymous cache cannot independently authorize a move.
use super::*;
use crate::migration_review::{InventoryScratch, storage::scratch::Allocation};
use std::os::unix::fs::FileExt;
pub(super) struct Done {
    file: Allocation,
    scratch: Arc<InventoryScratch>,
    entries: usize,
    bytes: u64,
    count: usize,
}
impl Done {
    pub(super) fn new(entries: usize, scratch: Arc<InventoryScratch>) -> io::Result<Self> {
        let bytes = u64::try_from(entries)
            .map_err(io::Error::other)?
            .checked_add(7)
            .ok_or_else(|| changed("Adoption checkpoint index overflow"))?
            / 8;
        let file = scratch.allocate(bytes)?;
        // The newly allocated anonymous inode has no previous authority/data.
        Ok(Self {
            file,
            scratch,
            entries,
            bytes,
            count: 0,
        })
    }
    pub(super) fn len(&self) -> usize {
        self.count
    }
    fn bit(&self, index: usize) -> io::Result<(u64, u8, u8)> {
        self.scratch.check_custody()?;
        if index >= self.entries || self.file.file.metadata()?.len() != self.bytes {
            return Err(changed("Adoption checkpoint index changed"));
        }
        let offset = u64::try_from(index / 8).map_err(io::Error::other)?;
        let mask = 1u8 << (index % 8);
        let mut byte = [0];
        self.file.file.read_exact_at(&mut byte, offset)?;
        Ok((offset, mask, byte[0]))
    }
    pub(super) fn contains(&self, index: usize) -> io::Result<bool> {
        let (_, mask, byte) = self.bit(index)?;
        Ok(byte & mask != 0)
    }
    pub(super) fn insert(&mut self, index: usize) -> io::Result<bool> {
        let (offset, mask, byte) = self.bit(index)?;
        if byte & mask != 0 {
            return Ok(false);
        }
        self.file.file.write_all_at(&[byte | mask], offset)?;
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| changed("Adoption checkpoint count overflow"))?;
        Ok(true)
    }
}
