//! Where a thin package's payload blobs come from.
//!
//! A registry serves its reviewed payload blobs over HTTPS ([`HttpBlobs`]) or
//! from a directory on this machine ([`LocalBlobs`], see
//! [`registry_client::local`]). Both are content addressed and verify every
//! object against its name; neither is trusted for what it returns.

use std::io;

use anyhow::{Result, bail};
use tilo_app_package::{BlobFetcher, BlobHash, BlobSink};

use crate::http_blobs::HttpBlobs;
use crate::local_blobs::LocalBlobs;

/// A registry's payload blobs, by the registry's base URL.
#[derive(Clone, Debug)]
pub enum RegistryBlobs {
    /// A registry reached over HTTPS (or loopback HTTP for development).
    Http(HttpBlobs),
    /// A registry directory on this machine, `file://<path>`.
    Directory(LocalBlobs),
}

impl RegistryBlobs {
    /// The blobs a registry at `base_url` serves.
    ///
    /// # Errors
    ///
    /// Returns an error for a `file://` URL that names no absolute directory,
    /// or any scheme other than `file`, `http` or `https`.
    pub fn parse(base_url: &str) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/');
        if let Some(path) = base_url.strip_prefix("file://") {
            if path.is_empty() || !std::path::Path::new(path).is_absolute() {
                bail!("a local registry URL names an absolute directory: file://<path>");
            }
            return Ok(Self::Directory(LocalBlobs::new(path)));
        }
        if base_url.starts_with("https://") || base_url.starts_with("http://") {
            return Ok(Self::Http(HttpBlobs::new(base_url)));
        }
        bail!("a registry URL is https://… or file://<absolute directory>")
    }

    /// The registry's base URL.
    pub fn base_url(&self) -> String {
        match self {
            Self::Http(http) => http.base_url().to_owned(),
            Self::Directory(local) => {
                format!("file://{}", local.root().display())
            }
        }
    }
}

impl BlobFetcher for RegistryBlobs {
    fn fetch(&self, hashes: &[BlobHash], sink: &mut dyn BlobSink) -> io::Result<()> {
        match self {
            Self::Http(http) => http.fetch(hashes, sink),
            Self::Directory(local) => local.fetch(hashes, sink),
        }
    }
}
