//! Parser-free administrative read protocol. Serializable messages confer no device authority.
use serde::{Deserialize, Serialize};
pub const SCHEMA: u32 = 2;
pub const FRAME_BYTES: usize = 65_536;
pub const CHUNK_BYTES: u32 = 1_048_576;
pub const HASH_BYTES: u64 = 1_073_741_824;
pub type NativePath = Vec<Vec<u16>>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub volume: String,
    pub root: String,
    pub upcase: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub token: String,
    pub path: NativePath,
    pub key: NativePath,
    pub record: u64,
    pub sequence: u16,
    pub size: u64,
    pub directory: bool,
    pub metadata_digest: String,
    pub content_readable: bool,
    pub enumeration_complete: bool,
    pub fidelity_pending: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub path: Option<NativePath>,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: u32,
    pub nonce: String,
    pub id: u64,
    pub operation: Operation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum Operation {
    Hello,
    BeginObservation,
    NextManifestPage,
    ReadChunk {
        token: String,
        offset: u64,
        length: u32,
    },
    FinishObservation,
    Cancel,
    Close,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub schema: u32,
    pub nonce: String,
    pub id: u64,
    pub result: Response,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", deny_unknown_fields)]
pub enum Response {
    Hello {
        identity: Identity,
        client_uid: u32,
        backend: String,
        acceptance: String,
    },
    Begun {
        generation: u64,
    },
    Page {
        entries: Vec<Entry>,
        issues: Vec<Issue>,
        upcase: Vec<u16>,
        done: bool,
    },
    Data {
        offset: u32,
        bytes: Vec<u8>,
    },
    ReadDone {
        length: u32,
        sha256: String,
    },
    Finished {
        hashes: Vec<(String, String)>,
        issues: Vec<Issue>,
        done: bool,
    },
    Closed,
}
#[cfg(target_os = "linux")]
pub mod client;
#[cfg(target_os = "linux")]
pub mod socket;
#[derive(Default)]
pub struct Sequence {
    next: u64,
    invalid: bool,
}
impl Sequence {
    /// # Errors
    /// Permanently invalidates stale, skipped or wrong-session requests.
    pub fn accept(&mut self, r: &Request, nonce: &str) -> std::io::Result<()> {
        if self.invalid
            || r.schema != SCHEMA
            || r.nonce != nonce
            || r.id != self.next
            || self.next == u64::MAX
        {
            self.invalid = true;
            return Err(std::io::Error::other("session sequence invalidated"));
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("request overflow"))?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_request_permanently_invalidates_session() {
        let mut s = Sequence::default();
        let r = Request {
            schema: 2,
            nonce: "selected".into(),
            id: 0,
            operation: Operation::Hello,
        };
        s.accept(&r, "selected").unwrap();
        assert!(s.accept(&r, "selected").is_err());
        let r = Request { id: 1, ..r };
        assert!(s.accept(&r, "selected").is_err());
    }
    #[test]
    fn foreign_nonce_cannot_authorize() {
        let mut s = Sequence::default();
        let r = Request {
            schema: 2,
            nonce: "other".into(),
            id: 0,
            operation: Operation::Hello,
        };
        assert!(s.accept(&r, "selected").is_err());
    }
}
