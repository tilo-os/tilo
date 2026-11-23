//! Provisional whole-file hashing, separate from permission to return a valid range.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
#[derive(Default)]
pub(crate) struct Stream {
    pub hashes: BTreeMap<String, String>,
    active: Option<(String, u64, Sha256)>,
}
impl Stream {
    pub fn contiguous(&self, token: &str, offset: u64) -> bool {
        self.active
            .as_ref()
            .is_some_and(|(t, at, _)| t == token && *at == offset)
    }
    pub fn clear_active(&mut self) {
        self.active = None;
    }
    pub fn active(&self) -> bool {
        self.active.is_some()
    }
    pub fn consume(&mut self, token: &str, size: u64, offset: u64, bytes: &[u8]) {
        self.hashes.remove(token);
        if !self.contiguous(token, offset) {
            self.active = None;
        }
        if offset == 0 {
            self.active = Some((token.to_owned(), 0, Sha256::new()));
        }
        if let Some((_, position, hash)) = &mut self.active {
            let Some(end) = position.checked_add(bytes.len() as u64) else {
                self.active = None;
                return;
            };
            if end > size {
                self.active = None;
                return;
            }
            hash.update(bytes);
            *position = end;
            if end == size {
                self.hashes
                    .insert(token.to_owned(), hex::encode(hash.clone().finalize()));
                self.active = None;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jumps_overlaps_switches_and_restart_keep_hash_completion_precise() {
        let mut stream = Stream::default();
        stream.consume("a", 6, 0, b"abc");
        assert!(stream.contiguous("a", 3));
        stream.consume("a", 6, 4, b"ef");
        assert!(!stream.active());
        assert!(!stream.hashes.contains_key("a"));
        stream.consume("a", 6, 0, b"abc");
        stream.consume("a", 6, 2, b"cde");
        assert!(!stream.hashes.contains_key("a"));
        stream.consume("a", 6, 0, b"abc");
        stream.consume("b", 3, 1, b"bc");
        assert!(!stream.active());
        assert!(stream.hashes.is_empty());
        stream.consume("a", 6, 0, b"abc");
        stream.consume("a", 6, 3, b"def");
        assert_eq!(
            stream.hashes.get("a").unwrap(),
            "bef57ec7f53a6d40beb640a780a639c83bc29ac8a9816f1fc6c5c6dcd93c4721"
        );
        stream.consume("a", 6, 1, b"b");
        assert!(!stream.hashes.contains_key("a"));
        stream.consume("b", 6, 0, b"abc");
        stream.clear_active();
        assert!(!stream.active());
    }
}
