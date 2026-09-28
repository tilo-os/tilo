//! Upload sessions: one streamed `PUT`, or a Nextcloud chunked upload.
//!
//! Nextcloud's chunked upload (v2) stages numbered chunks in
//! `remote.php/dav/uploads/<user>/<transfer>/` and assembles them with a
//! final `MOVE` of the virtual `.file` onto the destination. The staging
//! folder survives restarts, so a session resumes from the chunks the
//! server already holds.

use cloud_source::{
    CloudSource, Error, Item, ItemId, Result, Revision, UploadSession, UploadState,
};
use serde::{Deserialize, Serialize};
use tilo_dav::{Body, Depth, Precondition, StatusCode, Url};

use crate::{DavSource, paths};

/// The precondition of the final write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Condition {
    /// Replace; with a revision, only if the file is still at it.
    Match(Option<Revision>),
    /// Create; never replace.
    Create,
}

impl Condition {
    fn precondition(&self) -> Precondition<'_> {
        match self {
            Self::Match(Some(revision)) => Precondition::IfMatch(revision.as_str()),
            Self::Match(None) => Precondition::None,
            Self::Create => Precondition::IfNoneMatchAny,
        }
    }

    /// The final assembly `MOVE`'s headers for this condition. Plain
    /// `If-Match`/`If-None-Match` would test the request URI, the virtual
    /// `.file`, which always exists; the destination is tested with
    /// `Overwrite` and an `If` header tagged with its URL.
    fn assembly_headers(&self, destination: &Url) -> Vec<(&'static str, String)> {
        match self {
            Self::Create => vec![("Overwrite", "F".to_owned())],
            Self::Match(None) => vec![("Overwrite", "T".to_owned())],
            Self::Match(Some(revision)) => vec![
                ("Overwrite", "T".to_owned()),
                ("If", format!("<{destination}> ([{}])", revision.as_str())),
            ],
        }
    }

    fn failed(&self) -> Error {
        match self {
            Self::Create => Error::AlreadyExists,
            Self::Match(_) => Error::Conflict { current: None },
        }
    }
}

/// A whole file in one streamed `PUT`.
pub struct SinglePut {
    source: DavSource,
    id: ItemId,
    condition: Condition,
    replaced: Option<Revision>,
    size: u64,
    offset: u64,
}

impl SinglePut {
    /// `replaced` is the revision the write overwrites, where the source
    /// has to make sure the overwrite yields a new one (see
    /// `new_nextcloud_revision`).
    pub fn new(
        source: &DavSource,
        id: ItemId,
        condition: Condition,
        replaced: Option<Revision>,
        size: u64,
    ) -> Self {
        Self {
            source: source.clone(),
            id,
            condition,
            replaced,
            size,
            offset: 0,
        }
    }

    fn put(&mut self, body: Body) -> Result<()> {
        let url = self.source.url(&self.id, false)?;
        let response = self
            .source
            .client()?
            .send("PUT", url, &[], body, self.condition.precondition())
            .map_err(|_| Error::Offline)?;
        match response.status() {
            status if status.is_success() => {
                self.offset = self.size;
                Ok(())
            }
            StatusCode::PRECONDITION_FAILED => Err(self.condition.failed()),
            StatusCode::CONFLICT => Err(Error::NotFound),
            _ => Err(self.source.status_error(&response)),
        }
    }
}

impl UploadSession for SinglePut {
    fn offset(&self) -> u64 {
        self.offset
    }

    fn state(&self) -> Option<UploadState> {
        None
    }

    fn write_chunk(&mut self, data: Box<dyn std::io::Read + Send>, len: u64) -> Result<()> {
        if self.offset != 0 || len != self.size {
            return Err(Error::Unsupported("a plain WebDAV upload is one chunk"));
        }
        self.put(Body::Reader(data, len))
    }

    fn finish(mut self: Box<Self>) -> Result<Item> {
        if self.size == 0 && self.offset == 0 {
            self.put(Body::Empty)?;
        }
        if self.offset < self.size {
            return Err(Error::InvalidResponse("upload finished early"));
        }
        let item = self.source.stat(&self.id)?;
        new_nextcloud_revision(&self.source, &self.id, self.replaced.as_ref(), item)
    }

    fn abort(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ChunkedState {
    transfer: String,
    id: ItemId,
    condition: Condition,
    #[serde(default)]
    replaced: Option<Revision>,
    size: u64,
    chunks: u32,
    offset: u64,
}

/// A Nextcloud chunked upload.
pub struct Chunked {
    source: DavSource,
    state: ChunkedState,
}

impl Chunked {
    pub fn begin(
        source: &DavSource,
        id: ItemId,
        condition: Condition,
        replaced: Option<Revision>,
        size: u64,
    ) -> Result<Self> {
        let chunked = Self {
            source: source.clone(),
            state: ChunkedState {
                transfer: format!("tilo-{}", uuid::Uuid::new_v4().simple()),
                id,
                condition,
                replaced,
                size,
                chunks: 0,
                offset: 0,
            },
        };
        let destination = chunked.destination()?;
        let response = source
            .client()?
            .send(
                "MKCOL",
                chunked.folder()?,
                &[("Destination", destination.as_str())],
                Body::Empty,
                Precondition::None,
            )
            .map_err(|_| Error::Offline)?;
        if !response.status().is_success() {
            return Err(source.status_error(&response));
        }
        Ok(chunked)
    }

    pub fn resume(source: &DavSource, state: &UploadState) -> Result<Self> {
        let state: ChunkedState =
            serde_json::from_str(&state.0).map_err(|_| Error::SessionExpired)?;
        let mut chunked = Self {
            source: source.clone(),
            state,
        };
        let folder = chunked.folder()?;
        let listing = match source.propfind(folder.clone(), Depth::One) {
            Ok(listing) => listing,
            Err(Error::NotFound) => return Err(Error::SessionExpired),
            Err(error) => return Err(error),
        };
        let mut chunks = 0_u32;
        let mut offset = 0_u64;
        for response in listing.responses.iter().filter(|r| !r.is_collection()) {
            let Some(name) = paths::id_for_href(&folder, &response.href) else {
                continue;
            };
            if name.as_str().parse::<u32>().is_err() {
                continue;
            }
            chunks = chunks.saturating_add(1);
            offset = offset.saturating_add(
                response
                    .prop_text("getcontentlength")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
            );
        }
        chunked.state.chunks = chunks;
        chunked.state.offset = offset;
        Ok(chunked)
    }

    fn folder(&self) -> Result<Url> {
        self.source
            .config
            .nextcloud_url("uploads", &[self.state.transfer.as_str()], true)
            .ok_or(Error::Unsupported("chunked upload"))
    }

    fn file(&self, name: &str) -> Result<Url> {
        self.source
            .config
            .nextcloud_url("uploads", &[self.state.transfer.as_str(), name], false)
            .ok_or(Error::Unsupported("chunked upload"))
    }

    fn destination(&self) -> Result<Url> {
        self.source.url(&self.state.id, false)
    }
}

impl UploadSession for Chunked {
    fn offset(&self) -> u64 {
        self.state.offset
    }

    fn state(&self) -> Option<UploadState> {
        serde_json::to_string(&self.state).ok().map(UploadState)
    }

    fn write_chunk(&mut self, data: Box<dyn std::io::Read + Send>, len: u64) -> Result<()> {
        let index = self.state.chunks.saturating_add(1);
        let url = self.file(&format!("{index:05}"))?;
        let destination = self.destination()?;
        let total = self.state.size.to_string();
        let response = self
            .source
            .client()?
            .send(
                "PUT",
                url,
                &[
                    ("Destination", destination.as_str()),
                    ("OC-Total-Length", total.as_str()),
                ],
                Body::Reader(data, len),
                Precondition::None,
            )
            .map_err(|_| Error::Offline)?;
        match response.status() {
            status if status.is_success() => {
                self.state.chunks = index;
                self.state.offset = self.state.offset.saturating_add(len);
                Ok(())
            }
            StatusCode::NOT_FOUND => Err(Error::SessionExpired),
            _ => Err(self.source.status_error(&response)),
        }
    }

    fn finish(self: Box<Self>) -> Result<Item> {
        if self.state.offset < self.state.size {
            return Err(Error::InvalidResponse("upload finished early"));
        }
        let destination = self.destination()?;
        let total = self.state.size.to_string();
        let condition = self.state.condition.assembly_headers(&destination);
        let mut headers = vec![
            ("Destination", destination.as_str()),
            ("OC-Total-Length", total.as_str()),
        ];
        headers.extend(
            condition
                .iter()
                .map(|(name, value)| (*name, value.as_str())),
        );
        let response = self
            .source
            .client()?
            .send(
                "MOVE",
                self.file(".file")?,
                &headers,
                Body::Empty,
                Precondition::None,
            )
            .map_err(|_| Error::Offline)?;
        match response.status() {
            status if status.is_success() => {
                let item = self.source.stat(&self.state.id)?;
                new_nextcloud_revision(
                    &self.source,
                    &self.state.id,
                    self.state.replaced.as_ref(),
                    item,
                )
            }
            StatusCode::PRECONDITION_FAILED => Err(self.state.condition.failed()),
            StatusCode::NOT_FOUND => Err(Error::SessionExpired),
            _ => Err(self.source.status_error(&response)),
        }
    }

    fn abort(self: Box<Self>) -> Result<()> {
        let response = self
            .source
            .client()?
            .delete(self.folder()?, Precondition::None)
            .map_err(|_| Error::Offline)?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(self.source.status_error(&response))
        }
    }
}

/// Make an overwrite of `replaced` end at a new revision.
///
/// Nextcloud keeps a file's `ETag` when an overwrite lands in the same
/// second as the write before it: its scanner reuses the cached `ETag` while
/// the storage mtime is unchanged, even though the content changed. Handing
/// that `ETag` back as the new revision would let a later `If-Match` on the
/// old content pass. Setting the mtime one second past the stale one makes
/// Nextcloud issue a fresh `ETag`; the touch is itself conditional on the
/// stale `ETag`, so it never lands on someone else's newer write.
pub(crate) fn new_nextcloud_revision(
    source: &DavSource,
    id: &ItemId,
    replaced: Option<&Revision>,
    item: Item,
) -> Result<Item> {
    let Some(replaced) = replaced else {
        return Ok(item);
    };
    if item.revision.as_ref() != Some(replaced) {
        return Ok(item);
    }
    let mtime = item
        .modified
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_secs());
    let body = format!(
        "<?xml version=\"1.0\"?><d:propertyupdate xmlns:d=\"DAV:\"><d:set><d:prop>\
         <d:lastmodified>{}</d:lastmodified></d:prop></d:set></d:propertyupdate>",
        mtime.saturating_add(1)
    );
    let response = source
        .client()?
        .send(
            "PROPPATCH",
            source.url(id, false)?,
            &[("Content-Type", "application/xml; charset=utf-8")],
            Body::Bytes(body.into_bytes()),
            Precondition::IfMatch(replaced.as_str()),
        )
        .map_err(|_| Error::Offline)?;
    if !response.status().is_success() && response.status() != StatusCode::PRECONDITION_FAILED {
        return Err(source.status_error(&response));
    }
    let item = source.stat(id)?;
    if item.revision.as_ref() == Some(replaced) {
        return Err(Error::InvalidResponse(
            "stale Nextcloud revision after overwrite",
        ));
    }
    Ok(item)
}
