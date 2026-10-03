//! Uploads: one `files/upload` for a file that fits a single chunk, else an
//! upload session (`upload_session/start`, `append_v2`, `finish`) that can
//! be resumed from its persisted state after a restart.
//!
//! Every request carries the `content_hash` of its body, so Dropbox rejects
//! bytes damaged in transit, and the committed file's hash is compared with
//! the hash of everything sent.

use std::io::Read;

use cloud_source::{Error, Item, Result, UploadSession, UploadState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::Failure;
use crate::content_hash::{BLOCK, ContentHasher, content_hash};
use crate::{DropboxSource, MAX_UPLOAD_REQUEST};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Mode {
    Add,
    Overwrite,
    Update(String),
}

/// Where the upload is committed and under which write mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    path: String,
    mode: Mode,
}

impl Commit {
    /// Create; an existing item at `path` is a conflict.
    pub fn add(path: String) -> Self {
        Self {
            path,
            mode: Mode::Add,
        }
    }

    /// Replace unconditionally.
    pub fn overwrite(path: String) -> Self {
        Self {
            path,
            mode: Mode::Overwrite,
        }
    }

    /// Replace only while the file is at `rev`.
    pub fn update(path: String, rev: String) -> Self {
        Self {
            path,
            mode: Mode::Update(rev),
        }
    }

    fn arg(&self) -> Value {
        let mode = match &self.mode {
            Mode::Add => json!({".tag": "add"}),
            Mode::Overwrite => json!({".tag": "overwrite"}),
            Mode::Update(rev) => json!({".tag": "update", "update": rev}),
        };
        json!({
            "path": self.path,
            "mode": mode,
            "autorename": false,
            "mute": false,
            // Conflict even when the target holds identical bytes or was
            // deleted since `rev`: a precondition is a precondition.
            "strict_conflict": true,
        })
    }

    fn error(&self, failure: Failure) -> Error {
        if failure.has("conflict") {
            return match self.mode {
                Mode::Update(_) => Error::Conflict { current: None },
                Mode::Add | Mode::Overwrite => Error::AlreadyExists,
            };
        }
        session_error(failure)
    }
}

/// Errors of the session calls: a session Dropbox no longer knows (or that
/// is closed, or whose offset disagrees beyond repair) must start again.
/// Commit errors (`path/…`) keep their meaning.
fn session_error(failure: Failure) -> Error {
    let expired = matches!(
        failure.first_tag(),
        Some("lookup_failed" | "not_found" | "closed" | "not_closed" | "incorrect_offset")
    );
    if expired && !failure.has("content_hash_mismatch") {
        Error::SessionExpired
    } else {
        failure.into_error()
    }
}

/// The server's committed offset from an `incorrect_offset` error.
fn correct_offset(failure: &Failure) -> Option<u64> {
    let Failure::Route { error, .. } = failure else {
        return None;
    };
    error
        .get("correct_offset")
        .or_else(|| error.get("lookup_failed")?.get("correct_offset"))
        .and_then(Value::as_u64)
}

#[derive(Serialize, Deserialize)]
struct Saved {
    session: String,
    offset: u64,
    size: u64,
    commit: Commit,
    /// Block digests of the bytes before `offset`, when it is on a 4 MiB
    /// boundary, so the resumed upload still verifies the whole file.
    blocks: Option<Vec<String>>,
}

pub struct Upload {
    source: DropboxSource,
    commit: Commit,
    size: u64,
    offset: u64,
    session: Option<String>,
    /// `None` once the bytes before `offset` are unknown (resumed off a
    /// block boundary); the whole-file check is then skipped.
    hasher: Option<ContentHasher>,
    committed: Option<Value>,
}

impl Upload {
    pub fn begin(source: DropboxSource, commit: Commit, size: u64) -> Self {
        Self {
            source,
            commit,
            size,
            offset: 0,
            session: None,
            hasher: Some(ContentHasher::new()),
            committed: None,
        }
    }

    pub fn resume(source: DropboxSource, state: &UploadState) -> Result<Self> {
        let saved: Saved = serde_json::from_str(&state.0).map_err(|_| Error::SessionExpired)?;
        let hasher = saved
            .blocks
            .as_deref()
            .filter(|_| saved.offset.is_multiple_of(BLOCK as u64))
            .and_then(ContentHasher::from_blocks);
        Ok(Self {
            source,
            commit: saved.commit,
            size: saved.size,
            offset: saved.offset,
            session: Some(saved.session),
            hasher,
            committed: None,
        })
    }

    fn accept(&mut self, chunk: &[u8]) {
        if let Some(hasher) = self.hasher.as_mut() {
            hasher.update(chunk);
        }
        self.offset = self.offset.saturating_add(chunk.len() as u64);
    }

    fn append(&mut self, session: &str, chunk: &[u8]) -> Result<()> {
        let arg = json!({
            "cursor": {"session_id": session, "offset": self.offset},
            "close": false,
            "content_hash": content_hash(chunk),
        });
        let len = chunk.len() as u64;
        match self
            .source
            .api
            .upload("files/upload_session/append_v2", &arg, chunk.to_vec())
        {
            Ok(_) => {
                self.accept(chunk);
                Ok(())
            }
            Err(failure) if failure.has("incorrect_offset") => {
                match correct_offset(&failure) {
                    // An earlier attempt of this very chunk was applied
                    // (its response was lost before the state was saved).
                    Some(correct) if Some(correct) == self.offset.checked_add(len) => {
                        self.accept(chunk);
                        Ok(())
                    }
                    // The server holds more than we sent from here on;
                    // continue from its offset, unverified.
                    Some(correct) if correct > self.offset => {
                        self.offset = correct;
                        self.hasher = None;
                        Ok(())
                    }
                    _ => Err(Error::SessionExpired),
                }
            }
            Err(failure) => Err(session_error(failure)),
        }
    }

    fn verify(&self, value: &Value) -> Result<()> {
        let Some(hasher) = self.hasher.clone() else {
            return Ok(());
        };
        let committed = value.get("content_hash").and_then(Value::as_str);
        match committed {
            Some(hash) if !hash.eq_ignore_ascii_case(&hasher.finish()) => {
                Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the committed file does not match the uploaded bytes",
                )))
            }
            _ => Ok(()),
        }
    }

    fn upload_whole(&self, body: Vec<u8>) -> Result<Value> {
        let mut arg = self.commit.arg();
        if let Some(object) = arg.as_object_mut() {
            object.insert("content_hash".into(), json!(content_hash(&body)));
        }
        self.source
            .api
            .upload("files/upload", &arg, body)
            .map_err(|failure| self.commit.error(failure))
    }
}

impl UploadSession for Upload {
    fn offset(&self) -> u64 {
        self.offset
    }

    fn state(&self) -> Option<UploadState> {
        let session = self.session.clone()?;
        let saved = Saved {
            session,
            offset: self.offset,
            size: self.size,
            commit: self.commit.clone(),
            blocks: self
                .hasher
                .as_ref()
                .and_then(ContentHasher::boundary_blocks),
        };
        serde_json::to_string(&saved).ok().map(UploadState)
    }

    fn write_chunk(&mut self, data: Box<dyn Read + Send>, len: u64) -> Result<()> {
        if self.committed.is_some() {
            return Err(Error::InvalidResponse("the upload is already committed"));
        }
        if len > MAX_UPLOAD_REQUEST {
            return Err(Error::TooLarge);
        }
        let mut chunk = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        data.take(len).read_to_end(&mut chunk)?;
        if chunk.len() as u64 != len {
            return Err(Error::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        match self.session.clone() {
            None if self.offset == 0 && len == self.size => {
                let value = self.upload_whole(chunk.clone())?;
                self.accept(&chunk);
                self.committed = Some(value);
                Ok(())
            }
            None => {
                let arg = json!({"close": false, "content_hash": content_hash(&chunk)});
                let value = self
                    .source
                    .api
                    .upload("files/upload_session/start", &arg, chunk.clone())
                    .map_err(session_error)?;
                let session = value
                    .get("session_id")
                    .and_then(Value::as_str)
                    .ok_or(Error::InvalidResponse("session_id"))?;
                self.session = Some(session.to_owned());
                self.accept(&chunk);
                Ok(())
            }
            Some(session) => self.append(&session, &chunk),
        }
    }

    fn finish(self: Box<Self>) -> Result<Item> {
        let value = match (&self.committed, &self.session) {
            (Some(value), _) => value.clone(),
            (None, None) if self.size == 0 => self.upload_whole(Vec::new())?,
            (None, Some(session)) if self.offset == self.size => {
                let mut arg = json!({
                    "cursor": {"session_id": session, "offset": self.offset},
                    "commit": self.commit.arg(),
                });
                if let (Some(hasher), Some(object)) = (self.hasher.clone(), arg.as_object_mut()) {
                    object.insert("content_hash".into(), json!(hasher.finish()));
                }
                self.source
                    .api
                    .upload("files/upload_session/finish", &arg, Vec::new())
                    .map_err(|failure| self.commit.error(failure))?
            }
            _ => return Err(Error::InvalidResponse("the upload is incomplete")),
        };
        self.verify(&value)?;
        self.source.file_item(&value)
    }

    fn abort(self: Box<Self>) -> Result<()> {
        // Dropbox has no call to discard a session; unfinished sessions
        // expire on their own after a week.
        Ok(())
    }
}
