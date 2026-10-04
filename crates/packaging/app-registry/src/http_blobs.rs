//! Payload blobs from a registry server over HTTPS.
//!
//! A registry server keeps the payload blobs of the packages it reviewed
//! and serves each at `<base>/blobs/<blake3 hex>`. An installer holding a
//! thin package fetches from there only the blobs its store lacks. The
//! server is not trusted: every blob is checked against its hash, and a
//! response is cut off past a size ceiling so a hostile server cannot make
//! the client buffer without bound.

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use tilo_app_package::{BlobFetcher, BlobHash, BlobSink, PackageFetcher, PackageId};

/// No single payload blob is larger than an application's whole
/// materialised payload budget allows, with room for non-app packages.
const MAX_BLOB_BYTES: u64 = 64 * 1024 * 1024;

/// A registry server serving payload blobs at `<base>/blobs/<hash>`.
#[derive(Clone, Debug)]
pub struct HttpBlobs {
    base_url: String,
}

impl HttpBlobs {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Fetch every blob in `hashes`, in order, each checked against its hash.
    ///
    /// # Errors
    ///
    /// Returns an error if a request fails, the server lacks a blob, a
    /// response exceeds the size ceiling, or bytes do not match their hash.
    pub async fn fetch_all(&self, hashes: &[BlobHash]) -> Result<Vec<Bytes>> {
        let client = tilo_net::http::client()
            .build()
            .context("build registry HTTP client")?;
        let mut out = Vec::with_capacity(hashes.len());
        for blob in hashes {
            let url = format!("{}/blobs/{blob}", self.base_url);
            let mut response = client
                .get(&url)
                .send()
                .await
                .with_context(|| format!("GET {url}"))?
                .error_for_status()
                .with_context(|| format!("GET {url}"))?;
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .with_context(|| format!("read {url}"))?
            {
                body.extend_from_slice(&chunk);
                if body.len() as u64 > MAX_BLOB_BYTES {
                    bail!("{url} exceeds the {MAX_BLOB_BYTES}-byte payload blob ceiling");
                }
            }
            if blake3::hash(&body).as_bytes() != blob.as_bytes() {
                return Err(anyhow!(
                    "{url} served bytes that do not hash to the payload blob"
                ));
            }
            out.push(Bytes::from(body));
        }
        Ok(out)
    }
}

impl HttpBlobs {
    /// The reviewed package `id` in its thin form, served at
    /// `<base>/thin/<id hex>.tapp`, or `None` when the registry has no
    /// such package. The bytes are not trusted: the caller checks they
    /// form package `id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response exceeds the
    /// size ceiling.
    pub async fn fetch_thin(&self, id: &PackageId) -> Result<Option<Bytes>> {
        let client = tilo_net::http::client()
            .build()
            .context("build registry HTTP client")?;
        let url = format!("{}/thin/{}.tapp", self.base_url, id.to_hex());
        let mut response = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        response = response
            .error_for_status()
            .with_context(|| format!("GET {url}"))?;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .with_context(|| format!("read {url}"))?
        {
            body.extend_from_slice(&chunk);
            if body.len() as u64 > MAX_BLOB_BYTES {
                bail!("{url} exceeds the {MAX_BLOB_BYTES}-byte package ceiling");
            }
        }
        Ok(Some(Bytes::from(body)))
    }

    /// Run `work` on a private current-thread runtime; installers call
    /// in from synchronous code, where nesting a runtime would deadlock.
    fn block_on<T>(work: impl std::future::Future<Output = Result<T>>) -> std::io::Result<T> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(std::io::Error::other(
                "registry fetch must not run on an async runtime thread",
            ));
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(work)
            .map_err(std::io::Error::other)
    }
}

impl PackageFetcher for HttpBlobs {
    fn fetch_package(&self, id: &PackageId) -> std::io::Result<Option<Vec<u8>>> {
        Self::block_on(self.fetch_thin(id)).map(|bytes| bytes.map(|bytes| bytes.to_vec()))
    }
}

impl BlobFetcher for HttpBlobs {
    fn fetch(&self, hashes: &[BlobHash], sink: &mut dyn BlobSink) -> std::io::Result<()> {
        let blobs = Self::block_on(self.fetch_all(hashes))?;
        for (hash, bytes) in hashes.iter().zip(blobs) {
            sink.accept(hash, bytes.len() as u64, &mut bytes.as_ref())?;
        }
        Ok(())
    }
}
