//! Content a caller hands its workspace directly, kept on the device's shelf: what the content put
//! API writes into, and what a get reads back out.
//!
//! The bytes land under [`CONTENT_PUT_SCOPE`], no app's own, so every app of the workspace reads
//! them once an action hands it their reference. The broker decides who may put
//! ([`crate::apps::collab_app_broker::AppBrokerState::content_puts`]); a get answers only from
//! the store of the workspace the caller is in.

use std::sync::Arc;

use tilo_actions::remote::{
    ActionCallError, Caller, ContentGets, ContentPiece, MAX_CONTENT_FRAME_BYTES,
};
use tilo_broker_core::services::{CONTENT_PUT_SCOPE, ContentSink, ContentStoreServices};
use tilo_content::hash_bytes;

use super::{ContentScope, ContentShelf, MAX_CONTENT_BYTES};

/// Where `workspace`'s put content lives on the shelf.
fn put_scope(workspace: &str) -> ContentScope {
    ContentScope {
        workspace: workspace.to_owned(),
        app: CONTENT_PUT_SCOPE.to_owned(),
    }
}

/// The content put API's store over `shelf`.
#[must_use]
pub fn shelf_puts(shelf: Arc<dyn ContentShelf>) -> Arc<dyn ContentStoreServices> {
    Arc::new(ShelfPuts(shelf))
}

/// Content gets over `shelf`: the put content of the caller's workspace.
#[must_use]
pub fn shelf_gets(shelf: Arc<dyn ContentShelf>) -> Arc<dyn ContentGets> {
    Arc::new(ShelfGets(shelf))
}

struct ShelfPuts(Arc<dyn ContentShelf>);

impl ContentStoreServices for ShelfPuts {
    fn max_bytes(&self) -> u64 {
        MAX_CONTENT_BYTES
    }

    fn begin(&self, workspace: &str) -> Result<Box<dyn ContentSink>, String> {
        let scope = put_scope(workspace);
        if !scope.names_a_store() {
            return Err("this workspace name keeps no content".into());
        }
        Ok(Box::new(Upload {
            shelf: Arc::clone(&self.0),
            scope,
            bytes: Vec::new(),
        }))
    }

    fn holds(&self, workspace: &str, hash: &str) -> bool {
        self.0.has(&put_scope(workspace), hash)
    }
}

/// One upload still arriving.
struct Upload {
    shelf: Arc<dyn ContentShelf>,
    scope: ContentScope,
    bytes: Vec<u8>,
}

impl ContentSink for Upload {
    fn append(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.bytes.len().saturating_add(bytes.len()) as u64 > MAX_CONTENT_BYTES {
            return Err("content is larger than a record may reference".into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<(String, u64), String> {
        let Self {
            shelf,
            scope,
            bytes,
        } = *self;
        let hash = hash_bytes(&bytes);
        shelf.keep(&scope, &hash, &bytes)?;
        Ok((hash, bytes.len() as u64))
    }
}

struct ShelfGets(Arc<dyn ContentShelf>);

impl ContentGets for ShelfGets {
    fn get(
        &self,
        caller: &Caller,
        content: &str,
        offset: u64,
        length: u64,
    ) -> Result<ContentPiece, ActionCallError> {
        let invalid = |reason: String| ActionCallError::InvalidRequest { reason };
        let workspace = caller.workspace().ok_or_else(|| {
            invalid("content is read from a workspace, and this caller is in none".into())
        })?;
        let hash = tilo_actions::content_hash(content)
            .ok_or_else(|| invalid(format!("{content} is not a tilo.content:<hash> reference")))?;
        // The length asked is the most the caller takes: a piece carries at most what a frame
        // does, and a caller that asked for more reads on from where the piece ends, as it does
        // with any piece shorter than the content.
        let most = MAX_CONTENT_FRAME_BYTES as u64;
        let length = match length {
            0 => most,
            length => length.min(most),
        };
        let bytes = self
            .0
            .read(&put_scope(workspace), hash)
            .ok_or_else(|| invalid(format!("this workspace has no {content}")))?;
        let size = bytes.len() as u64;
        if offset > size {
            return Err(invalid(format!(
                "the content is {size} bytes: an offset of {offset} is past its end"
            )));
        }
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let end = usize::try_from(offset.saturating_add(length).min(size)).unwrap_or(usize::MAX);
        Ok(ContentPiece {
            size,
            offset,
            bytes: bytes.get(start..end).unwrap_or_default().to_vec(),
        })
    }
}
