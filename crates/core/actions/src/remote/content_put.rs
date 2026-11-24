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

use super::{ActionCallError, Caller};

/// The most raw bytes one append carries. MCP requests are bounded at
/// 1 MiB; 512 KiB is about 683 KiB of base64, leaving room for the rest of
/// the request.
pub const MAX_CONTENT_PUT_CHUNK: usize = 512 * 1024;

/// How far ahead of the bytes accepted so far one append may land: a client
/// sends a group of [`MAX_CONTENT_PUT_CHUNK`]-sized chunks at once and a slow
/// link then costs one round trip per group rather than per chunk. The
/// receiver holds the early ones and appends them as the gap before them
/// fills; the `written` an answer carries is the bytes held contiguously from
/// the start. Eight chunks is as many as a client sends in a group.
///
/// The frames a chunk is split into travel the same way
/// ([`super::ContentPut`]): a session hands a device every frame of an upload
/// before it waits for the first answer, so an upload costs a round trip per
/// window of frames, not one per frame.
pub const MAX_CONTENT_PUT_AHEAD: usize = 8 * MAX_CONTENT_PUT_CHUNK;

/// The most raw bytes one many-files step carries (hex doubles them, staying
/// within [`super::MAX_CONTENT_FRAME_BYTES`]).
pub const MAX_PUT_MANY_BYTES: usize = 384 * 1024;

/// The most files one many-files step carries: every one is a store write,
/// and a step answers within the peer's reply deadline.
pub const MAX_PUT_MANY_FILES: usize = 256;

/// The most hashes one step asks about at once. Every hash costs the store a
/// look, and one step must answer within the peer's reply deadline: a few
/// thousand is minutes of looking on a loaded disk, so a caller asks in
/// steps this size instead.
pub const MAX_HAVE_HASHES: usize = 1_024;

/// Uploads one session keeps open at once: a client putting a folder keeps
/// one open per file until it finishes, so it may hold as many as a workspace
/// keeps for all its callers ([`super::MAX_HOSTED_CONTENT_PUTS`]).
pub const MAX_OPEN_CONTENT_PUTS: usize = 64;

/// What a provider without a content store answers.
pub const CONTENT_PUTS_UNAVAILABLE: &str = "content uploads are not available here";

/// The scheme of a content reference: what a put answers for the content it stored.
pub const CONTENT_REFERENCE: &str = "tilo.content:";

/// The content hash (lowercase BLAKE3 hex) `address` names: a reference as a put answers it
/// (`tilo.content:<hash>`), or the bare hash of bytes a caller hashed itself. `None` for
/// anything else.
#[must_use]
pub fn content_hash(address: &str) -> Option<&str> {
    let hash = address.strip_prefix(CONTENT_REFERENCE).unwrap_or(address);
    (hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(hash)
}

/// A provider's API for content puts.
pub trait ContentPuts: Send + Sync {
    /// Open an upload of exactly `size` bytes into the content store of
    /// `caller`'s workspace.
    fn begin(&self, caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError>;

    /// Store `files` whole, answering each one's reference in order: what a
    /// folder of small files needs, one call instead of three a file.
    ///
    /// # Errors
    /// What [`Self::begin`] or a file's store refused.
    fn put_many(&self, caller: &Caller, files: &[Vec<u8>]) -> Result<Vec<String>, ActionCallError> {
        let mut contents = Vec::with_capacity(files.len());
        for bytes in files {
            let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let mut put = self.begin(caller, size)?;
            put.append(0, bytes)?;
            contents.push(put.finish()?);
        }
        Ok(contents)
    }

    /// Which of `hashes` the content store of `caller`'s workspace already
    /// holds, in order: each a reference a put answered, or a bare hash
    /// ([`content_hash`]). One that names no content is not held.
    ///
    /// # Errors
    /// A store that cannot be asked, or content `caller` may not read.
    fn have(&self, caller: &Caller, hashes: &[String]) -> Result<Vec<bool>, ActionCallError> {
        let _ = (caller, hashes);
        Err(ActionCallError::Unavailable {
            reason: "this workspace's content cannot be asked what it holds".into(),
        })
    }

    /// Store `put`, which `caller` may have begun on a connection that has since ended, and
    /// answer its reference.
    ///
    /// # Errors
    /// What the put refused ([`ContentPut::finish_as`]).
    fn finish(&self, caller: &Caller, put: Box<dyn ContentPut>) -> Result<String, ActionCallError> {
        put.finish_as(caller)
    }
}

/// One upload in progress, owned by the session that began it: dropped
/// unfinished (the session ended), it stores nothing.
pub trait ContentPut: Send {
    /// The size the upload began with.
    fn size(&self) -> u64;

    /// Bytes accepted so far. An upload that reaches another device over a
    /// session hands its pieces over without waiting for each answer, so it
    /// answers the bytes handed over: what a piece may land after, and what
    /// its finish verifies every piece of.
    fn written(&self) -> u64;

    /// Accept `chunk` at `offset`, answering the bytes accepted so far.
    /// `offset` must be exactly what was accepted so far, so a chunk sent
    /// again is refused, never stored twice; the refusal names the offset
    /// to resume at.
    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError>;

    /// Store the content and answer its reference, `tilo.content:<hash>`.
    fn finish(self: Box<Self>) -> Result<String, ActionCallError>;

    /// [`Self::finish`], decided as `caller`: the same person or app as began the upload,
    /// finishing it on a later connection of theirs.
    ///
    /// # Errors
    /// What [`Self::finish`] refuses, or a caller other than the one that began the upload.
    fn finish_as(self: Box<Self>, caller: &Caller) -> Result<String, ActionCallError> {
        let _ = caller;
        self.finish()
    }
}

/// Open uploads one provider keeps for all its callers at once; past it, the
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
    /// Store several whole files at once, each hex-encoded: the answer names
    /// each one's reference in order ([`ContentPutAnswer::Many`]).
    Many { files: Vec<String> },
    /// Ask which of these hashes (lowercase BLAKE3 hex) the store already
    /// holds ([`ContentPutAnswer::Have`]).
    Have { hashes: Vec<String> },
    /// Where the upload stands: answered with the bytes accepted so far
    /// ([`ContentPutAnswer::Appended`]), so a client whose append failed on
    /// the way resumes from there.
    Progress { upload: u64 },
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
    /// The reference of each file stored, in the order asked.
    Many { contents: Vec<String> },
    /// Whether each hash asked about is held, in the order asked.
    Have { present: Vec<bool> },
}

/// The uploads a provider keeps open for the callers that began them over a
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
    /// Where `caller`'s open upload `upload` stands, if they have one so called.
    fn progress(&self, caller: &Caller, upload: u64) -> Option<ContentPutAnswer> {
        use tilo_lock::MutexRecover as _;
        self.open
            .lock_recover()
            .uploads
            .get(&upload)
            .filter(|(owner, _, _)| owner == caller)
            .map(|(_, _, put)| ContentPutAnswer::Appended {
                written: put.written(),
            })
    }

    /// Store the files of a [`ContentPutStep::Many`].
    fn many(
        puts: &dyn ContentPuts,
        caller: &Caller,
        files: &[String],
    ) -> Result<ContentPutAnswer, ActionCallError> {
        if files.is_empty() {
            return Err(ActionCallError::InvalidRequest {
                reason: "store at least one file".into(),
            });
        }
        if files.len() > MAX_PUT_MANY_FILES {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("one call carries at most {MAX_PUT_MANY_FILES} files"),
            });
        }
        let mut decoded = Vec::with_capacity(files.len());
        let mut total = 0usize;
        for file in files {
            if file.len() > MAX_PUT_MANY_BYTES.saturating_mul(2) {
                return Err(ActionCallError::InvalidRequest {
                    reason: format!("one file carries at most {MAX_PUT_MANY_BYTES} bytes"),
                });
            }
            let bytes = hex::decode(file).map_err(|_| ActionCallError::InvalidRequest {
                reason: "a file's bytes must be hex".into(),
            })?;
            total = total.saturating_add(bytes.len());
            decoded.push(bytes);
        }
        if total > MAX_PUT_MANY_BYTES {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("one call carries at most {MAX_PUT_MANY_BYTES} bytes of files"),
            });
        }
        puts.put_many(caller, &decoded)
            .map(|contents| ContentPutAnswer::Many { contents })
    }

    /// Answer which of a [`ContentPutStep::Have`]'s addresses the store holds.
    fn have(
        puts: &dyn ContentPuts,
        caller: &Caller,
        hashes: &[String],
    ) -> Result<ContentPutAnswer, ActionCallError> {
        if hashes.len() > MAX_HAVE_HASHES {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("one call asks about at most {MAX_HAVE_HASHES} hashes"),
            });
        }
        let mut named = Vec::with_capacity(hashes.len());
        for address in hashes {
            let hash = content_hash(address).ok_or_else(|| ActionCallError::InvalidRequest {
                reason: format!(
                    "{address} names no content: neither a {CONTENT_REFERENCE} reference nor a \
                     lowercase BLAKE3 hash"
                ),
            })?;
            named.push(hash.to_owned());
        }
        puts.have(caller, &named)
            .map(|present| ContentPutAnswer::Have { present })
    }

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
    ) -> Result<ContentPutAnswer, ActionCallError> {
        use tilo_lock::MutexRecover as _;
        let gone = |upload: u64| ActionCallError::InvalidRequest {
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
                    return Err(ActionCallError::InvalidRequest {
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
                if bytes.len() > super::MAX_CONTENT_FRAME_BYTES.saturating_mul(2) {
                    return Err(ActionCallError::InvalidRequest {
                        reason: format!(
                            "a piece carries at most {} bytes",
                            super::MAX_CONTENT_FRAME_BYTES
                        ),
                    });
                }
                let chunk = hex::decode(bytes).map_err(|_| ActionCallError::InvalidRequest {
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
                if matches!(appended, Err(ActionCallError::Refused { .. })) {
                    // The store refused the bytes: the upload is over.
                    open.uploads.remove(&upload);
                }
                drop(open);
                appended.map(|written| ContentPutAnswer::Appended { written })
            }
            ContentPutStep::Progress { upload } => {
                self.progress(caller, upload).ok_or_else(|| gone(upload))
            }
            ContentPutStep::Many { files } => Self::many(puts, caller, &files),
            ContentPutStep::Have { hashes } => Self::have(puts, caller, &hashes),
            ContentPutStep::Finish { upload } => {
                let mut open = self.open.lock_recover();
                let (_, _, put) = open
                    .uploads
                    .get(&upload)
                    .filter(|(owner, _, _)| owner == caller)
                    .ok_or_else(|| gone(upload))?;
                // Short of its size, the upload stays open to be completed.
                if put.written() != put.size() {
                    return Err(ActionCallError::InvalidRequest {
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
                puts.finish(caller, put)
                    .map(|content| ContentPutAnswer::Finished { content })
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    /// Puts kept in memory, finished as the bytes' length.
    struct Memory;

    struct Held(u64, Vec<u8>);

    impl ContentPuts for Memory {
        fn begin(&self, _: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError> {
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
        fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
            if offset != self.written() {
                return Err(ActionCallError::InvalidRequest {
                    reason: "offset".into(),
                });
            }
            self.1.extend_from_slice(chunk);
            Ok(self.written())
        }
        fn finish(self: Box<Self>) -> Result<String, ActionCallError> {
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
        assert_eq!(
            puts.step(&Memory, &ada, ContentPutStep::append(upload, 0, b"ab"))
                .ok(),
            None,
            "a piece sent again is refused"
        );
        assert_eq!(
            puts.step(&Memory, &ada, ContentPutStep::Progress { upload })
                .unwrap(),
            ContentPutAnswer::Appended { written: 2 },
            "the upload says where to resume"
        );
        assert!(
            puts.step(&Memory, &eve, ContentPutStep::Progress { upload })
                .is_err(),
            "nobody else learns where it stands"
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

    /// A many-files step takes at most [`MAX_PUT_MANY_FILES`] files, so it
    /// answers within the peer's reply deadline however small they are.
    #[test]
    fn a_many_files_step_takes_at_most_its_files() {
        let puts = HostedContentPuts::default();
        let ada = Caller::local_person("ada");
        let whole = vec![String::new(); MAX_PUT_MANY_FILES];
        let ContentPutAnswer::Many { contents } = puts
            .step(&Memory, &ada, ContentPutStep::Many { files: whole })
            .unwrap()
        else {
            panic!("many")
        };
        assert_eq!(contents.len(), MAX_PUT_MANY_FILES);
        let past = vec![String::new(); MAX_PUT_MANY_FILES.saturating_add(1)];
        assert!(
            puts.step(&Memory, &ada, ContentPutStep::Many { files: past })
                .is_err(),
            "a step of more files is refused rather than made to wait"
        );
    }
}
