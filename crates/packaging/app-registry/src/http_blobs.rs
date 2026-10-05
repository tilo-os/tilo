//! Payload blobs from a registry server over HTTPS.
//!
//! A registry server keeps the payload blobs of the packages it reviewed
//! and serves each at `<base>/blobs/<blake3 hex>`. A blob larger than one
//! object is kept as chunks instead: `<base>/blobs/<blake3 hex>.chunks` is
//! its chunk list and every chunk is again a `<base>/blobs/<chunk hex>`
//! (see [`tilo_app_package::chunks`]). An installer holding a thin package
//! fetches from there only the blobs its store lacks.
//!
//! The server is not trusted: every object is checked against its hash, a
//! chunked blob's bytes reach the sink only chunk by verified chunk and its
//! final chunk only once the whole blob verified, and a response is cut off
//! past [`MAX_OBJECT_BYTES`] so a hostile server cannot make the client
//! buffer without bound. At most one object is held in memory.

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use tilo_app_package::chunks::{
    ChunkList, ChunkRef, ChunkedReader, MAX_CHUNK_LIST_BYTES, MAX_OBJECT_BYTES, Reassembly,
};
use tilo_app_package::{BlobFetcher, BlobHash, BlobSink, PackageFetcher, PackageId};

/// A registry server serving payload blobs at `<base>/blobs/<hash>`.
#[derive(Clone, Debug)]
pub struct HttpBlobs {
    base_url: String,
}

/// How the registry holds one payload blob.
enum Stored {
    Whole(Bytes),
    Chunked(ChunkList),
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

    fn client() -> Result<reqwest::Client> {
        tilo_net::http::client()
            .build()
            .context("build registry HTTP client")
    }

    /// GET `url`, or `None` on 404, refusing a body over `limit` bytes.
    async fn get_bounded(client: &reqwest::Client, url: &str, limit: u64) -> Result<Option<Bytes>> {
        let response = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let mut response = response
            .error_for_status()
            .with_context(|| format!("GET {url}"))?;
        if response.content_length().is_some_and(|len| len > limit) {
            bail!("{url} exceeds the {limit}-byte object ceiling");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .with_context(|| format!("read {url}"))?
        {
            body.extend_from_slice(&chunk);
            if body.len() as u64 > limit {
                bail!("{url} exceeds the {limit}-byte object ceiling");
            }
        }
        Ok(Some(Bytes::from(body)))
    }

    /// The whole blob `hash`, or its chunk list when the registry keeps it
    /// as chunks.
    async fn locate(&self, client: &reqwest::Client, hash: &BlobHash) -> Result<Stored> {
        let url = format!("{}/blobs/{hash}", self.base_url);
        if let Some(body) = Self::get_bounded(client, &url, MAX_OBJECT_BYTES).await? {
            if blake3::hash(&body).as_bytes() != hash.as_bytes() {
                return Err(anyhow!(
                    "{url} served bytes that do not hash to the payload blob"
                ));
            }
            return Ok(Stored::Whole(body));
        }
        let list_url = format!("{}/blobs/{hash}.chunks", self.base_url);
        let Some(list) = Self::get_bounded(client, &list_url, MAX_CHUNK_LIST_BYTES).await? else {
            bail!(
                "registry {} has payload blob {hash} neither whole nor as chunks",
                self.base_url
            );
        };
        let list = ChunkList::decode(&list, hash).with_context(|| format!("decode {list_url}"))?;
        Ok(Stored::Chunked(list))
    }

    /// One chunk's bytes, at most its listed length. The caller verifies
    /// them against the name.
    async fn fetch_chunk(&self, client: &reqwest::Client, chunk: &ChunkRef) -> Result<Bytes> {
        let url = format!("{}/blobs/{}", self.base_url, chunk.hash);
        Self::get_bounded(client, &url, chunk.len.min(MAX_OBJECT_BYTES))
            .await?
            .ok_or_else(|| anyhow!("registry {} lacks chunk {url}", self.base_url))
    }

    /// Fetch every blob in `hashes`, in order, each checked against its hash.
    /// Holds every blob in memory; installers stream through [`BlobFetcher`]
    /// instead.
    ///
    /// # Errors
    ///
    /// Returns an error if a request fails, the server lacks a blob, a
    /// response exceeds the size ceiling, or bytes do not match their hash.
    pub async fn fetch_all(&self, hashes: &[BlobHash]) -> Result<Vec<Bytes>> {
        let client = Self::client()?;
        let mut out = Vec::with_capacity(hashes.len());
        for hash in hashes {
            match self.locate(&client, hash).await? {
                Stored::Whole(bytes) => out.push(bytes),
                Stored::Chunked(list) => {
                    let mut assembly = Reassembly::new(list);
                    let mut blob = Vec::new();
                    while let Some(chunk) = assembly.current() {
                        let bytes = self.fetch_chunk(&client, &chunk).await?;
                        assembly.update(&bytes)?;
                        assembly.finish_chunk()?;
                        blob.extend_from_slice(&bytes);
                    }
                    out.push(Bytes::from(blob));
                }
            }
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
        let client = Self::client()?;
        let url = format!("{}/thin/{}.tapp", self.base_url, id.to_hex());
        Self::get_bounded(&client, &url, MAX_OBJECT_BYTES).await
    }

    /// A private current-thread runtime; installers call in from
    /// synchronous code, where nesting a runtime would deadlock.
    fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(std::io::Error::other(
                "registry fetch must not run on an async runtime thread",
            ));
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
    }

    /// Deliver one blob to `sink`: whole, or chunk by verified chunk.
    fn deliver(
        &self,
        runtime: &tokio::runtime::Runtime,
        client: &reqwest::Client,
        hash: &BlobHash,
        sink: &mut dyn BlobSink,
    ) -> std::io::Result<()> {
        let stored = runtime
            .block_on(self.locate(client, hash))
            .map_err(std::io::Error::other)?;
        match stored {
            Stored::Whole(bytes) => sink.accept(hash, bytes.len() as u64, &mut bytes.as_ref()),
            Stored::Chunked(list) => {
                let len = list.len;
                let mut reader = ChunkedReader::new(list, |chunk: &ChunkRef| {
                    runtime
                        .block_on(self.fetch_chunk(client, chunk))
                        .map(|bytes| bytes.to_vec())
                        .map_err(std::io::Error::other)
                });
                sink.accept(hash, len, &mut reader)?;
                // A sink that stopped early still gets no unverified blob:
                // finish verifying what it did not read.
                std::io::copy(&mut reader, &mut std::io::sink())?;
                if !reader.is_complete() {
                    return Err(std::io::Error::other(format!(
                        "payload blob {hash} did not complete"
                    )));
                }
                Ok(())
            }
        }
    }
}

impl PackageFetcher for HttpBlobs {
    fn fetch_package(&self, id: &PackageId) -> std::io::Result<Option<Vec<u8>>> {
        Self::runtime()?
            .block_on(self.fetch_thin(id))
            .map(|bytes| bytes.map(|bytes| bytes.to_vec()))
            .map_err(std::io::Error::other)
    }
}

impl BlobFetcher for HttpBlobs {
    fn fetch(&self, hashes: &[BlobHash], sink: &mut dyn BlobSink) -> std::io::Result<()> {
        let runtime = Self::runtime()?;
        let client = Self::client().map_err(std::io::Error::other)?;
        for hash in hashes {
            self.deliver(&runtime, &client, hash, sink)?;
        }
        Ok(())
    }
}
