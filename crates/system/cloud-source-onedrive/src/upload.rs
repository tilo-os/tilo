//! Uploads: Graph upload sessions for any file with a body, a plain `PUT`
//! for empty files (a session needs at least one byte).
//!
//! A session is created with its precondition, `If-Match` for a replace and
//! `conflictBehavior: fail` for a create. Fragments go to the session's
//! pre-authenticated URL without the token, in multiples of 320 KiB; the
//! last one commits the file. Graph may only detect a name clash at that
//! commit, so the final fragment can still fail with
//! [`Error::AlreadyExists`]. The session URL is the resumable state.

use std::io::Read;

use cloud_source::{Error, Item, Result, UploadSession, UploadState, UploadTarget};
use reqwest::blocking::Body;
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, IF_MATCH};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::graph::{DriveItem, UploadSessionInfo};
use crate::http::{self, Auth, Http};
use crate::{FRAGMENT_UNIT, OneDriveSource};

/// What [`UploadState`] holds.
#[derive(Serialize, Deserialize)]
struct Saved {
    url: String,
    size: u64,
}

pub fn begin(
    source: &OneDriveSource,
    target: UploadTarget,
    size: u64,
) -> Result<Box<dyn UploadSession>> {
    if size == 0 {
        return Ok(Box::new(Empty {
            source: source.clone(),
            target,
        }));
    }
    let (url, body, expect) = match &target {
        UploadTarget::Replace { id, expect } => (
            format!("{}/createUploadSession", source.item_url(id.as_str())),
            serde_json::json!({"item": {"@microsoft.graph.conflictBehavior": "replace"}}),
            expect.clone(),
        ),
        UploadTarget::Create { parent, name } => (
            format!(
                "{}/createUploadSession",
                source.child_url(parent.as_str(), name)?
            ),
            serde_json::json!({"item": {
                "@microsoft.graph.conflictBehavior": "fail",
                "name": name,
            }}),
            None,
        ),
    };
    let mut request = source
        .http
        .request(Method::POST, &url)
        .header(CONTENT_TYPE, "application/json")
        .body(body.to_string());
    if let Some(expect) = &expect {
        request = request.header(IF_MATCH, expect.as_str());
    }
    let info: UploadSessionInfo = source.http.json(request, Auth::Bearer)?;
    let url = info
        .upload_url
        .clone()
        .ok_or(Error::InvalidResponse("upload session URL"))?;
    Ok(Box::new(Session {
        source: source.clone(),
        url,
        size,
        offset: info.next_offset().unwrap_or(0),
        done: None,
    }))
}

pub fn resume(source: &OneDriveSource, state: &UploadState) -> Result<Box<dyn UploadSession>> {
    let saved: Saved = serde_json::from_str(&state.0).map_err(|_| Error::SessionExpired)?;
    let mut session = Session {
        source: source.clone(),
        url: saved.url,
        size: saved.size,
        offset: 0,
        done: None,
    };
    session.offset = session.status()?;
    Ok(Box::new(session))
}

/// A Graph upload session.
struct Session {
    source: OneDriveSource,
    url: String,
    size: u64,
    offset: u64,
    done: Option<Item>,
}

impl Session {
    fn http(&self) -> &Http {
        &self.source.http
    }

    /// Where the server expects the next byte.
    fn status(&self) -> Result<u64> {
        let response = self.http().send_raw(
            self.http().request(Method::GET, &self.url),
            Auth::Preauthenticated,
        )?;
        if !response.status().is_success() {
            return Err(self.session_error(http::failure(response)));
        }
        let info: UploadSessionInfo = http::read_json(response)?;
        info.next_offset()
            .ok_or(Error::InvalidResponse("upload session status"))
    }

    fn session_error(&self, failure: http::Failure) -> Error {
        match failure.status {
            StatusCode::NOT_FOUND
            | StatusCode::GONE
            | StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN => Error::SessionExpired,
            _ => self.http().error(failure, Auth::Preauthenticated),
        }
    }
}

impl UploadSession for Session {
    fn offset(&self) -> u64 {
        self.offset
    }

    fn state(&self) -> Option<UploadState> {
        let saved = Saved {
            url: self.url.clone(),
            size: self.size,
        };
        serde_json::to_string(&saved).ok().map(UploadState)
    }

    fn write_chunk(&mut self, data: Box<dyn Read + Send>, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let start = self.offset;
        let end = start
            .checked_add(len)
            .ok_or(Error::InvalidResponse("upload length"))?;
        if end > self.size {
            return Err(Error::InvalidResponse("chunk beyond the declared size"));
        }
        if end < self.size && !len.is_multiple_of(FRAGMENT_UNIT) {
            return Err(Error::Unsupported(
                "upload fragments other than multiples of 320 KiB",
            ));
        }
        let request = self
            .http()
            .request(Method::PUT, &self.url)
            .header(
                CONTENT_RANGE,
                format!("bytes {start}-{}/{}", end.saturating_sub(1), self.size),
            )
            .body(Body::sized(data, len));
        let response = self.http().send_raw(request, Auth::Preauthenticated)?;
        match response.status() {
            StatusCode::OK | StatusCode::CREATED => {
                let item: DriveItem = http::read_json(response)?;
                self.done = Some(self.source.to_item(&item)?);
                self.offset = self.size;
                Ok(())
            }
            StatusCode::ACCEPTED => {
                let info: UploadSessionInfo = http::read_json(response)?;
                self.offset = info.next_offset().unwrap_or(end);
                Ok(())
            }
            // A fragment the server already has (a retry after a lost
            // response): continue from where it actually is.
            StatusCode::RANGE_NOT_SATISFIABLE => {
                let _ = http::failure(response);
                let offset = self.status()?;
                if offset <= start {
                    return Err(Error::InvalidResponse("upload range"));
                }
                self.offset = offset;
                Ok(())
            }
            _ => Err(self.session_error(http::failure(response))),
        }
    }

    fn finish(self: Box<Self>) -> Result<Item> {
        self.done
            .ok_or(Error::InvalidResponse("the upload is incomplete"))
    }

    fn abort(self: Box<Self>) -> Result<()> {
        if self.done.is_some() {
            return Ok(());
        }
        let response = self.http().send_raw(
            self.http().request(Method::DELETE, &self.url),
            Auth::Preauthenticated,
        )?;
        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::NOT_FOUND | StatusCode::GONE => Ok(()),
            _ => Err(self.session_error(http::failure(response))),
        }
    }
}

/// An empty file: one `PUT` of no bytes at [`UploadSession::finish`].
struct Empty {
    source: OneDriveSource,
    target: UploadTarget,
}

impl UploadSession for Empty {
    fn offset(&self) -> u64 {
        0
    }

    fn state(&self) -> Option<UploadState> {
        None
    }

    fn write_chunk(&mut self, _data: Box<dyn Read + Send>, len: u64) -> Result<()> {
        if len == 0 {
            Ok(())
        } else {
            Err(Error::InvalidResponse("chunk beyond the declared size"))
        }
    }

    fn finish(self: Box<Self>) -> Result<Item> {
        let source = &self.source;
        let request = match &self.target {
            UploadTarget::Replace { id, expect } => {
                let request = source.http.request(
                    Method::PUT,
                    &format!("{}/content", source.item_url(id.as_str())),
                );
                OneDriveSource::if_match(request, expect.as_ref())
            }
            UploadTarget::Create { parent, name } => source.http.request(
                Method::PUT,
                &format!(
                    "{}/content?@microsoft.graph.conflictBehavior=fail",
                    source.child_url(parent.as_str(), name)?
                ),
            ),
        };
        let request = request
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(Vec::new());
        let item: DriveItem = source.http.json(request, Auth::Bearer)?;
        source.to_item(&item)
    }

    fn abort(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}
