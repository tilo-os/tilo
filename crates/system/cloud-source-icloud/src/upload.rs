//! Uploads: `upload/web` reserves a document and a content URL, the bytes go
//! to the content server in one multipart `POST`, and `update/documents`
//! commits them under a name in the parent folder.
//!
//! Replacing a file commits the new bytes to the existing document id, so
//! the item keeps its identity. Should Apple refuse that, or answer with a
//! different document, the old file is moved to the trash and the new one
//! takes its name: the bytes are replaced either way, only the id changes.

use std::io::{Cursor, Read};

use cloud_source::{
    CloudSource, Error, Item, ItemId, Result, UploadSession, UploadState, UploadTarget,
};
use reqwest::blocking::Body;
use serde_json::{Value, json};

use crate::api::{self, split_id};
use crate::{ICloudDrive, Service, is_icloud_host, now_millis, read_json, valid_name};

pub struct Upload {
    source: ICloudDrive,
    parent: ItemId,
    name: String,
    zone: String,
    mime: &'static str,
    /// The file being replaced, as it was when the upload began.
    replace: Option<Item>,
    expect: Option<cloud_source::Revision>,
    slot: api::UploadSlot,
    size: u64,
    uploaded: Option<api::SingleFile>,
}

impl Upload {
    pub fn begin(source: &ICloudDrive, target: UploadTarget, size: u64) -> Result<Self> {
        let (parent, name, replace, expect) = match target {
            UploadTarget::Create { parent, name } => {
                valid_name(&name)?;
                if source.find_child(&parent, &name)?.is_some() {
                    return Err(Error::AlreadyExists);
                }
                (parent, name, None, None)
            }
            UploadTarget::Replace { id, expect } => {
                let item = source.stat(&id)?;
                if !item.is_file() {
                    return Err(Error::NotFound);
                }
                if let Some(expected) = &expect
                    && item.revision.as_ref() != Some(expected)
                {
                    return Err(Error::Conflict {
                        current: item.revision,
                    });
                }
                let parent = item
                    .parents
                    .first()
                    .cloned()
                    .ok_or(Error::InvalidResponse("a file without a parent"))?;
                (parent, item.name.clone(), Some(item), expect)
            }
        };
        let zone = split_id(parent.as_str()).1.to_owned();
        let mime = mime_for(&name);
        let mut url = source.url(&Service::Docs, &format!("ws/{zone}/upload/web"))?;
        if let Some(token) = source.validate_token()? {
            url.query_pairs_mut().append_pair("token", &token);
        }
        let slots: Vec<api::UploadSlot> = source.post_url(
            url,
            &json!({"filename": name, "type": "FILE", "content_type": mime, "size": size}),
        )?;
        let slot = slots
            .into_iter()
            .next()
            .filter(|slot| !slot.url.is_empty() && !slot.document_id.is_empty())
            .ok_or(Error::InvalidResponse("no upload slot"))?;
        Ok(Self {
            source: source.clone(),
            parent,
            name,
            zone,
            mime,
            replace,
            expect,
            slot,
            size,
            uploaded: None,
        })
    }

    /// `POST` the bytes as `multipart/form-data`, streamed.
    fn send_content(&mut self, data: Box<dyn Read + Send>, len: u64) -> Result<()> {
        let url =
            url::Url::parse(&self.slot.url).map_err(|_| Error::InvalidResponse("upload url"))?;
        let boundary = format!("tilo-{}", uuid::Uuid::new_v4().simple());
        let filename: String = self
            .name
            .chars()
            .map(|c| match c {
                '"' => '\'',
                '\r' | '\n' => ' ',
                other => other,
            })
            .collect();
        let head = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{filename}\"\r\nContent-Type: {}\r\n\r\n",
            self.mime
        );
        let tail = format!("\r\n--{boundary}--\r\n");
        let total = (head.len() as u64)
            .saturating_add(len)
            .saturating_add(tail.len() as u64);
        let body = Cursor::new(head.into_bytes())
            .chain(data.take(len))
            .chain(Cursor::new(tail.into_bytes()));
        let request = self
            .source
            .inner
            .http
            .post(url.clone())
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::sized(body, total));
        let response = if is_icloud_host(&url) {
            let session = self.source.session()?;
            self.source.send(&session, request)?
        } else {
            let response = request.send().map_err(|_| Error::Offline)?;
            if !response.status().is_success() {
                return Err(self
                    .source
                    .status_error(response.status(), response.headers()));
            }
            response
        };
        let uploaded: api::Uploaded = read_json(response)?;
        let single = uploaded
            .single_file
            .ok_or(Error::InvalidResponse("no upload receipt"))?;
        if single.size != len {
            return Err(Error::InvalidResponse(
                "the content server stored a different size",
            ));
        }
        self.uploaded = Some(single);
        Ok(())
    }

    /// Commit the uploaded bytes as document `document`; the id Apple
    /// answers with.
    fn commit(&self, document: &str, single: &api::SingleFile) -> Result<String> {
        let mut data = json!({
            "signature": single.file_checksum,
            "wrapping_key": single.wrapping_key,
            "reference_signature": single.reference_checksum,
            "size": single.size,
        });
        if let (Some(receipt), Value::Object(map)) = (&single.receipt, &mut data) {
            map.insert("receipt".into(), Value::String(receipt.clone()));
        }
        let now = now_millis();
        let body = json!({
            "data": data,
            "command": "add_file",
            "create_short_guid": true,
            "document_id": document,
            "path": {
                "starting_document_id": split_id(self.parent.as_str()).2,
                "path": self.name,
            },
            "allow_conflict": true,
            "file_flags": {"is_writable": true, "is_executable": false, "is_hidden": false},
            "mtime": now,
            "btime": now,
        });
        let url = self.source.url(
            &Service::Docs,
            &format!("ws/{}/update/documents", self.zone),
        )?;
        let answer: api::UpdateResponse = self.source.post_url(url, &body)?;
        let result = answer.results.into_iter().next();
        match result {
            Some(api::UpdateResult {
                status,
                document: Some(committed),
            }) if answer.status.status_code == 0
                && status.status_code == 0
                && !committed.document_id.is_empty() =>
            {
                Ok(committed.document_id)
            }
            _ => Err(Error::InvalidResponse("document update refused")),
        }
    }

    fn file_id(&self, document: &str) -> ItemId {
        ItemId::new(format!("FILE::{}::{document}", self.zone))
    }

    /// Apple stored the new bytes as a separate document: retire the old
    /// file and give the new one its name.
    fn settle_replacement(&self, old: &Item, document: &str) -> Result<Item> {
        match self.source.stat(&old.id) {
            Ok(current) => {
                let etag = current.revision.map(|r| r.0).unwrap_or_default();
                self.source.trash(&old.id, &etag)?;
            }
            Err(Error::NotFound) => {}
            Err(error) => return Err(error),
        }
        let id = self.file_id(document);
        let created = self.source.stat(&id)?;
        if created.name != self.name {
            let etag = created.revision.map(|r| r.0).unwrap_or_default();
            self.source.rename(&id, &etag, &self.name)?;
        }
        self.source.stat(&id)
    }
}

impl UploadSession for Upload {
    fn offset(&self) -> u64 {
        if self.uploaded.is_some() {
            self.size
        } else {
            0
        }
    }

    fn state(&self) -> Option<UploadState> {
        None
    }

    fn write_chunk(&mut self, data: Box<dyn Read + Send>, len: u64) -> Result<()> {
        if self.uploaded.is_some() || len != self.size {
            return Err(Error::Unsupported("iCloud uploads are a single request"));
        }
        self.send_content(data, len)
    }

    fn finish(mut self: Box<Self>) -> Result<Item> {
        if self.uploaded.is_none() {
            if self.size != 0 {
                return Err(Error::InvalidResponse("the upload is incomplete"));
            }
            self.send_content(Box::new(std::io::empty()), 0)?;
        }
        let single = self
            .uploaded
            .clone()
            .ok_or(Error::InvalidResponse("no upload receipt"))?;
        let Some(old) = self.replace.clone() else {
            let document = self.commit(&self.slot.document_id, &single)?;
            return self.source.stat(&self.file_id(&document));
        };
        // Narrow the precondition's race to the commit itself.
        if let Some(expected) = &self.expect {
            let current = self.source.stat(&old.id)?;
            if current.revision.as_ref() != Some(expected) {
                return Err(Error::Conflict {
                    current: current.revision,
                });
            }
        }
        let old_document = split_id(old.id.as_str()).2.to_owned();
        match self.commit(&old_document, &single) {
            Ok(document) if document == old_document => self.source.stat(&old.id),
            Ok(document) => self.settle_replacement(&old, &document),
            Err(Error::InvalidResponse(_)) => {
                let document = self.commit(&self.slot.document_id, &single)?;
                self.settle_replacement(&old, &document)
            }
            Err(error) => Err(error),
        }
    }

    fn abort(self: Box<Self>) -> Result<()> {
        // Uncommitted content is never linked into the tree; Apple
        // discards it.
        Ok(())
    }
}

/// `upload/web` insists on a content type; by extension, with a generic
/// fallback.
fn mime_for(name: &str) -> &'static str {
    tilo_mime::from_path_or_octet_stream(name)
}
