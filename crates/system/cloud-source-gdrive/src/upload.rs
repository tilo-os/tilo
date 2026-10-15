//! Resumable uploads (`uploadType=resumable`).
//!
//! The session is opened with the file's metadata (`POST` to create,
//! `PATCH` to replace), then fed with `PUT`s carrying `Content-Range`.
//! Every chunk but the last is a multiple of [`UPLOAD_QUANTUM`]; the server
//! answers `308 Resume Incomplete` with the committed `Range`, and the file
//! resource once the last byte arrived. The session URI is the resumable
//! state: a status query (`Content-Range: bytes */<size>`) tells where to
//! continue after a restart.
//!
//! A replace with an expected revision re-checks the head revision right
//! before the request that commits, and that request carries at most one
//! quantum, so the unguarded window is one round trip plus 256 KiB.

use std::io::Read;
use std::sync::{Arc, Mutex};

use cloud_source::{
    Error, Item, ItemId, Result, Revision, UploadSession, UploadState, UploadTarget,
};
use reqwest::blocking::{Body, RequestBuilder, Response};
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, LOCATION};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::api::{self, File};
use crate::{Inner, UPLOAD_QUANTUM, Url};
use tilo_lock::MutexRecover as _;

/// Google's `499 Client Closed Request`, the answer to cancelling a session.
const CANCELLED: u16 = 499;
const RESUME_INCOMPLETE: u16 = 308;

#[derive(Debug, Serialize, Deserialize)]
struct Saved {
    #[serde(rename = "s")]
    session: String,
    #[serde(rename = "n")]
    size: u64,
    #[serde(rename = "i", default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(rename = "e", default, skip_serializing_if = "Option::is_none")]
    expect: Option<String>,
}

struct DriveUpload {
    inner: Arc<Inner>,
    session: Url,
    size: u64,
    offset: u64,
    /// The file being replaced.
    id: Option<String>,
    expect: Option<Revision>,
    /// The file resource the final chunk returned.
    done: Option<File>,
}

pub(crate) fn begin(
    inner: &Arc<Inner>,
    target: UploadTarget,
    size: u64,
) -> Result<Box<dyn UploadSession>> {
    let (method, segments, metadata, id, expect) = match target {
        UploadTarget::Create { parent, name } => {
            let parent = inner.real_id(&parent)?;
            (
                Method::POST,
                vec!["upload", "drive", "v3", "files"],
                serde_json::json!({ "name": name, "parents": [parent] }),
                None,
                None,
            )
        }
        UploadTarget::Replace { id, expect } => {
            // Fail early; the commit is checked again.
            inner.check_revision(&id, expect.as_ref())?;
            (
                Method::PATCH,
                vec!["upload", "drive", "v3", "files"],
                serde_json::json!({}),
                Some(inner.real_id(&id)?),
                expect,
            )
        }
    };
    let mut segments = segments;
    if let Some(id) = &id {
        segments.push(id.as_str());
    }
    let url = inner.url(
        &segments,
        &[
            ("uploadType", "resumable"),
            ("supportsAllDrives", "true"),
            ("fields", api::FILE_FIELDS),
        ],
    )?;
    let body = serde_json::to_vec(&metadata).map_err(|_| Error::InvalidResponse("json"))?;
    let builder = inner
        .request(method, url)?
        .header(CONTENT_TYPE, "application/json; charset=UTF-8")
        .header("X-Upload-Content-Length", size.to_string())
        .body(body);
    let response = inner.check(Inner::send(builder)?)?;
    let session = response
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Url::parse(v).ok())
        .ok_or(Error::InvalidResponse("upload session without a Location"))?;
    Ok(Box::new(DriveUpload {
        inner: Arc::clone(inner),
        session,
        size,
        offset: 0,
        id,
        expect,
        done: None,
    }))
}

pub(crate) fn resume(inner: &Arc<Inner>, state: &UploadState) -> Result<Box<dyn UploadSession>> {
    let saved: Saved = serde_json::from_str(&state.0).map_err(|_| Error::SessionExpired)?;
    let session = Url::parse(&saved.session).map_err(|_| Error::SessionExpired)?;
    let mut upload = DriveUpload {
        inner: Arc::clone(inner),
        session,
        size: saved.size,
        offset: 0,
        id: saved.id,
        expect: saved.expect.map(Revision::new),
        done: None,
    };
    // An empty upload has nothing to resume, and its status query would
    // commit it.
    if upload.size > 0 {
        let query = upload
            .put()?
            .header(CONTENT_RANGE, format!("bytes */{}", upload.size))
            .body(Vec::new());
        upload.absorb(query)?;
    }
    Ok(Box::new(upload))
}

/// Replace `id` with `size` bytes streamed from `body` (a restore).
pub(crate) fn restore(
    inner: &Arc<Inner>,
    id: &ItemId,
    size: u64,
    body: Box<dyn Read + Send>,
) -> Result<Item> {
    let chunk = inner.config.upload_chunk;
    let mut session = begin(
        inner,
        UploadTarget::Replace {
            id: id.clone(),
            expect: None,
        },
        size,
    )?;
    let shared = Shared(Arc::new(Mutex::new(body)));
    while session.offset() < size {
        let start = session.offset();
        let len = chunk.min(size.saturating_sub(start));
        session.write_chunk(Box::new(shared.clone().take(len)), len)?;
        if session.offset() != start.saturating_add(len) {
            // The body stream cannot be rewound to where the server stopped.
            let _ = session.abort();
            return Err(Error::InvalidResponse("the server kept part of a chunk"));
        }
    }
    session.finish()
}

/// One reader handed out in several consecutive request bodies.
#[derive(Clone)]
struct Shared(Arc<Mutex<Box<dyn Read + Send>>>);

impl Read for Shared {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.lock_recover().read(buf)
    }
}

impl DriveUpload {
    fn put(&self) -> Result<RequestBuilder> {
        let url = self.session.clone();
        if self.inner.same_origin(&url) {
            self.inner.request(Method::PUT, url)
        } else {
            // The session URI authorises itself; the token stays home.
            Ok(self.inner.http.request(Method::PUT, url))
        }
    }

    /// Send `len` bytes at the current offset.
    fn send_range(&mut self, body: Box<dyn Read + Send>, len: u64) -> Result<()> {
        let start = self.offset;
        let last = start
            .checked_add(len)
            .and_then(|end| end.checked_sub(1))
            .ok_or(Error::InvalidResponse("empty chunk"))?;
        let request = self
            .put()?
            .header(CONTENT_RANGE, format!("bytes {start}-{last}/{}", self.size))
            .body(Body::sized(body, len));
        self.absorb(request)
    }

    /// Send a chunk request and take the server's view of the session.
    fn absorb(&mut self, request: RequestBuilder) -> Result<()> {
        let response: Response = Inner::send(request)?;
        match response.status().as_u16() {
            200 | 201 => {
                self.done = Some(Inner::json(response)?);
                self.offset = self.size;
                Ok(())
            }
            RESUME_INCOMPLETE => {
                let range = response
                    .headers()
                    .get("range")
                    .and_then(|v| v.to_str().ok());
                self.offset = api::parse_committed(range)
                    .filter(|committed| *committed <= self.size)
                    .ok_or(Error::InvalidResponse("upload range"))?;
                Ok(())
            }
            404 | 410 => Err(Error::SessionExpired),
            _ => Err(self.inner.failure(response, false)),
        }
    }

    /// The precondition, immediately before the committing request.
    fn precondition(&self) -> Result<()> {
        let (Some(id), Some(expect)) = (&self.id, &self.expect) else {
            return Ok(());
        };
        match self
            .inner
            .check_revision(&ItemId::new(id.clone()), Some(expect))
        {
            Ok(_) => Ok(()),
            Err(error) => {
                let _ = self.cancel();
                Err(error)
            }
        }
    }

    fn cancel(&self) -> Result<()> {
        let url = self.session.clone();
        let builder = if self.inner.same_origin(&url) {
            self.inner.request(Method::DELETE, url)?
        } else {
            self.inner.http.request(Method::DELETE, url)
        };
        let response = Inner::send(builder)?;
        let status = response.status();
        if status.is_success()
            || status.as_u16() == CANCELLED
            || matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE)
        {
            Ok(())
        } else {
            Err(self.inner.failure(response, false))
        }
    }
}

impl UploadSession for DriveUpload {
    fn offset(&self) -> u64 {
        self.offset
    }

    fn state(&self) -> Option<UploadState> {
        let saved = Saved {
            session: self.session.to_string(),
            size: self.size,
            id: self.id.clone(),
            expect: self.expect.as_ref().map(|r| r.0.clone()),
        };
        serde_json::to_string(&saved).ok().map(UploadState)
    }

    fn write_chunk(&mut self, data: Box<dyn Read + Send>, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let end = self
            .offset
            .checked_add(len)
            .filter(|end| *end <= self.size)
            .ok_or(Error::InvalidResponse("chunk past the declared size"))?;
        if end < self.size || self.expect.is_none() {
            return self.send_range(data, len);
        }
        // The committing chunk of a conditional replace: send all but the
        // last quantum, check, then commit the rest.
        let head = (len.saturating_sub(1) / UPLOAD_QUANTUM).saturating_mul(UPLOAD_QUANTUM);
        let shared = Shared(Arc::new(Mutex::new(data)));
        if head > 0 {
            let start = self.offset;
            self.send_range(Box::new(shared.clone().take(head)), head)?;
            if self.offset != start.saturating_add(head) {
                // The server kept less; the caller re-sends from `offset`.
                return Ok(());
            }
        }
        self.precondition()?;
        let tail = len.saturating_sub(head);
        self.send_range(Box::new(shared.take(tail)), tail)
    }

    fn finish(mut self: Box<Self>) -> Result<Item> {
        if self.done.is_none() && self.size == 0 {
            self.precondition()?;
            let request = self
                .put()?
                .header(CONTENT_RANGE, "bytes */0")
                .body(Vec::new());
            self.absorb(request)?;
        }
        let file = self.done.take().ok_or(Error::InvalidResponse(
            "upload finished before its last byte",
        ))?;
        if let (Some(expect), Some(head)) = (&self.expect, &file.head_revision_id) {
            self.inner.verify_commit(&file.id, expect, head)?;
        }
        self.inner.to_item(&file)
    }

    fn abort(self: Box<Self>) -> Result<()> {
        self.cancel()
    }
}
