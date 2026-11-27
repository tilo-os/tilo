//! Content a caller hands its workspace directly, kept on the device's shelf: what the content put
//! API writes into, and what a get (`files::OpenedFiles`) reads back out.
//!
//! The bytes land under [`CONTENT_PUT_SCOPE`], no app's own, so every app of the workspace reads
//! them once an action hands it their reference. The broker decides who may put
//! (the app broker's `content_puts`).

use std::sync::Arc;

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

    /// Copied from the app's own scope, never another's: only what the app keeps reaches the
    /// workspace this way.
    fn hand_over(&self, workspace: &str, app: &str, hash: &str) -> Result<bool, String> {
        let put = put_scope(workspace);
        let own = ContentScope {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
        };
        if !put.names_a_store() || !own.names_a_store() {
            return Err(format!("{app} keeps no content in this workspace"));
        }
        if self.0.has(&put, hash) {
            return Ok(true);
        }
        let Some(bytes) = self.0.read(&own, hash) else {
            return Ok(false);
        };
        if bytes.len() as u64 > MAX_CONTENT_BYTES {
            return Err("content is larger than a record may reference".into());
        }
        self.0.keep(&put, hash, &bytes)?;
        Ok(true)
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
