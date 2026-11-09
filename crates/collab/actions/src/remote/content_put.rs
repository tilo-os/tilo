//! Content puts: how a session client hands bytes to its workspace's content
//! store, chunk by chunk, and gets back a reference to them.
//!
//! Actions carry intent and references; bytes travel by session transport
//! into the content store. A client (an AI agent through MCP, first) puts
//! the bytes, receives `tilo.content:<hash>` ([`crate::content_resource`])
//! and passes that reference to an action, which resolves it in the workspace.
//!
//! A put is invisible to everyone until an action at its own node
//! references it: storing content is never authority over any document.

use super::{ActionHostError, Caller};

/// The most raw bytes one append carries. MCP requests are bounded at
/// 1 MiB; 512 KiB is about 683 KiB of base64, leaving room for the rest of
/// the request.
pub const MAX_CONTENT_PUT_CHUNK: usize = 512 * 1024;

/// Uploads one session keeps open at once.
pub const MAX_OPEN_CONTENT_PUTS: usize = 4;

/// What a host without a content store answers.
pub const CONTENT_PUTS_UNAVAILABLE: &str = "content uploads are not available here";

/// A host's door for content puts.
pub trait ContentPuts: Send + Sync {
    /// Open an upload of exactly `size` bytes into the content store of
    /// `caller`'s workspace.
    fn begin(&self, caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionHostError>;
}

/// One upload in progress, owned by the session that began it: dropped
/// unfinished (the session ended), it stores nothing.
pub trait ContentPut: Send {
    /// The size the upload began with.
    fn size(&self) -> u64;

    /// Bytes accepted so far.
    fn written(&self) -> u64;

    /// Accept `chunk` at `offset`, answering the bytes accepted so far.
    /// `offset` must be exactly what was accepted so far, so a chunk sent
    /// again is refused, never stored twice; the refusal names the offset
    /// to resume at.
    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionHostError>;

    /// Store the content and answer its reference, `tilo.content:<hash>`.
    fn finish(self: Box<Self>) -> Result<String, ActionHostError>;
}

/// The most raw bytes one [`ContentPutStep::Append`] frame carries: hex
/// doubles them, and a frame stays within
/// [`super::MAX_REMOTE_ACTION_FRAME_BYTES`]. A client splits a larger chunk.
pub const MAX_CONTENT_PUT_FRAME_BYTES: usize = 384 * 1024;

/// Open uploads one host keeps for all its callers at once; past it, the
/// upload left untouched longest is dropped and stores nothing.
pub const MAX_HOSTED_CONTENT_PUTS: usize = 64;

/// One step of an upload asked of another device over a session
/// ([`super::RemoteActionMessage::ContentPutRequest`]): the same begin,
/// append and finish a local [`ContentPuts`] takes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentPutStep {
    /// Open an upload of exactly `size` bytes.
    Begin { size: u64 },
    /// Accept the next bytes at `offset`, hex-encoded so the frame stays
    /// self-describing text.
    Append {
        upload: u64,
        offset: u64,
        bytes: String,
    },
    /// Store what the upload holds.
    Finish { upload: u64 },
}

impl ContentPutStep {
    /// The append of `chunk` at `offset` to `upload`.
    #[must_use]
    pub fn append(upload: u64, offset: u64, chunk: &[u8]) -> Self {
        Self::Append {
            upload,
            offset,
            bytes: hex::encode(chunk),
        }
    }
}

/// What a device answers a [`ContentPutStep`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentPutAnswer {
    /// The upload is open under `upload`, for the caller who began it.
    Begun { upload: u64 },
    /// The bytes accepted so far.
    Appended { written: u64 },
    /// The content's reference, `tilo.content:<hash>`.
    Finished { content: String },
}

/// The uploads a host keeps open for the callers that began them over a
/// session, each answerable only to its own caller.
#[derive(Default)]
pub struct HostedContentPuts {
    open: std::sync::Mutex<HostedOpen>,
}

#[derive(Default)]
struct HostedOpen {
    last: u64,
    /// By id: who began it, when it was last touched, and the upload.
    uploads: std::collections::BTreeMap<u64, (Caller, u64, Box<dyn ContentPut>)>,
    touched: u64,
}

impl HostedContentPuts {
    /// Answer `step` from `caller`, putting into `puts`.
    ///
    /// # Errors
    ///
    /// What `puts` refused, an upload the caller has no such open, or bytes
    /// that are not hex.
    pub fn step(
        &self,
        puts: &dyn ContentPuts,
        caller: &Caller,
        step: ContentPutStep,
    ) -> Result<ContentPutAnswer, ActionHostError> {
        use tilo_lock::MutexRecover as _;
        let gone = |upload: u64| ActionHostError::InvalidRequest {
            reason: format!("no upload {upload} is open in this session"),
        };
        match step {
            ContentPutStep::Begin { size } => {
                let mut open = self.open.lock_recover();
                let mine = open
                    .uploads
                    .values()
                    .filter(|(owner, _, _)| owner == caller)
                    .count();
                if mine >= MAX_OPEN_CONTENT_PUTS {
                    return Err(ActionHostError::InvalidRequest {
                        reason: format!(
                            "this session already has {MAX_OPEN_CONTENT_PUTS} uploads open: finish one first"
                        ),
                    });
                }
                let begun = puts.begin(caller, size)?;
                while open.uploads.len() >= MAX_HOSTED_CONTENT_PUTS {
                    let Some(stalest) = open
                        .uploads
                        .iter()
                        .min_by_key(|(_, (_, touched, _))| *touched)
                        .map(|(id, _)| *id)
                    else {
                        break;
                    };
                    open.uploads.remove(&stalest);
                }
                open.last = open.last.saturating_add(1);
                open.touched = open.touched.saturating_add(1);
                let (upload, touched) = (open.last, open.touched);
                open.uploads
                    .insert(upload, (caller.clone(), touched, begun));
                drop(open);
                Ok(ContentPutAnswer::Begun { upload })
            }
            ContentPutStep::Append {
                upload,
                offset,
                bytes,
            } => {
                if bytes.len() > MAX_CONTENT_PUT_FRAME_BYTES.saturating_mul(2) {
                    return Err(ActionHostError::InvalidRequest {
                        reason: format!(
                            "a piece carries at most {MAX_CONTENT_PUT_FRAME_BYTES} bytes"
                        ),
                    });
                }
                let chunk = hex::decode(bytes).map_err(|_| ActionHostError::InvalidRequest {
                    reason: "a piece's bytes must be hex".into(),
                })?;
                let mut open = self.open.lock_recover();
                open.touched = open.touched.saturating_add(1);
                let now = open.touched;
                let entry = open
                    .uploads
                    .get_mut(&upload)
                    .filter(|(owner, _, _)| owner == caller)
                    .ok_or_else(|| gone(upload))?;
                entry.1 = now;
                let appended = entry.2.append(offset, &chunk);
                if matches!(appended, Err(ActionHostError::Refused { .. })) {
                    // The store refused the bytes: the upload is over.
                    open.uploads.remove(&upload);
                }
                drop(open);
                appended.map(|written| ContentPutAnswer::Appended { written })
            }
            ContentPutStep::Finish { upload } => {
                let mut open = self.open.lock_recover();
                let (_, _, put) = open
                    .uploads
                    .get(&upload)
                    .filter(|(owner, _, _)| owner == caller)
                    .ok_or_else(|| gone(upload))?;
                // Short of its size, the upload stays open to be completed.
                if put.written() != put.size() {
                    return Err(ActionHostError::InvalidRequest {
                        reason: format!(
                            "the upload has {} of {} bytes: append the rest at offset {}",
                            put.written(),
                            put.size(),
                            put.written()
                        ),
                    });
                }
                let (_, _, put) = open.uploads.remove(&upload).ok_or_else(|| gone(upload))?;
                drop(open);
                put.finish()
                    .map(|content| ContentPutAnswer::Finished { content })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    /// Puts kept in memory, finished as the bytes' length.
    struct Memory;

    struct Held(u64, Vec<u8>);

    impl ContentPuts for Memory {
        fn begin(&self, _: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionHostError> {
            Ok(Box::new(Held(size, Vec::new())))
        }
    }

    impl ContentPut for Held {
        fn size(&self) -> u64 {
            self.0
        }
        fn written(&self) -> u64 {
            u64::try_from(self.1.len()).unwrap()
        }
        fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionHostError> {
            if offset != self.written() {
                return Err(ActionHostError::InvalidRequest {
                    reason: "offset".into(),
                });
            }
            self.1.extend_from_slice(chunk);
            Ok(self.written())
        }
        fn finish(self: Box<Self>) -> Result<String, ActionHostError> {
            Ok(format!("{} bytes", self.1.len()))
        }
    }

    /// An upload over a session answers only the caller that began it,
    /// finishes only once whole, and is gone once finished.
    #[test]
    fn an_upload_is_its_callers_and_finishes_whole() {
        let puts = HostedContentPuts::default();
        let (ada, eve) = (Caller::local_person("ada"), Caller::local_person("eve"));
        let ContentPutAnswer::Begun { upload } = puts
            .step(&Memory, &ada, ContentPutStep::Begin { size: 4 })
            .unwrap()
        else {
            panic!("begun")
        };
        assert!(
            puts.step(&Memory, &eve, ContentPutStep::append(upload, 0, b"ab"))
                .is_err(),
            "another caller's upload is not there for it"
        );
        assert_eq!(
            puts.step(&Memory, &ada, ContentPutStep::append(upload, 0, b"ab"))
                .unwrap(),
            ContentPutAnswer::Appended { written: 2 }
        );
        assert!(
            puts.step(&Memory, &ada, ContentPutStep::Finish { upload })
                .is_err(),
            "half an upload does not finish"
        );
        puts.step(&Memory, &ada, ContentPutStep::append(upload, 2, b"cd"))
            .unwrap();
        assert_eq!(
            puts.step(&Memory, &ada, ContentPutStep::Finish { upload })
                .unwrap(),
            ContentPutAnswer::Finished {
                content: "4 bytes".into()
            }
        );
        assert!(
            puts.step(&Memory, &ada, ContentPutStep::Finish { upload })
                .is_err(),
            "a finished upload is closed"
        );
    }
}
