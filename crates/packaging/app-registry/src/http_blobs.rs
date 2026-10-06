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
use std::time::Duration;
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
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("build registry HTTP client")
    }

    /// GET `url`, or `None` on 404, refusing a body over `limit` bytes.
    async fn get_bounded(client: &reqwest::Client, url: &str, limit: u64) -> Result<Option<Bytes>> {
        Self::get_with_deadline(client, url, limit, Duration::from_secs(120)).await
    }

    async fn get_with_deadline(
        client: &reqwest::Client,
        url: &str,
        limit: u64,
        deadline: Duration,
    ) -> Result<Option<Bytes>> {
        const ATTEMPTS: u32 = 3;
        let mut attempt = 1;
        loop {
            match Self::get_once(client, url, limit, deadline).await {
                Ok(bytes) => return Ok(bytes),
                Err(error) => {
                    let transient = error.chain().any(|cause| {
                        cause.downcast_ref::<reqwest::Error>().is_some_and(|error| {
                            error.is_connect() || error.is_timeout() || error.is_body()
                        })
                    });
                    if !transient || attempt >= ATTEMPTS {
                        // The app CLI shows a bounded installer detail. Put the
                        // underlying cause before long content-addressed URLs.
                        let cause = error
                            .chain()
                            .last()
                            .map(ToString::to_string)
                            .unwrap_or_default();
                        return Err(anyhow!(
                            "{cause} (after {attempt} HTTP attempt(s)); {error:#}"
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(250 * u64::from(attempt))).await;
                    attempt = attempt.saturating_add(1);
                }
            }
        }
    }

    async fn get_once(
        client: &reqwest::Client,
        url: &str,
        limit: u64,
        deadline: Duration,
    ) -> Result<Option<Bytes>> {
        let response = client
            .get(url)
            .timeout(deadline)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    fn server(responses: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let count = responses.len();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0; 1024];
                assert!(stream.read(&mut request).unwrap() > 0);
                stream.write_all(&response).unwrap();
            }
            count
        });
        (format!("http://{address}"), worker)
    }

    fn response(length: usize, body: &[u8]) -> Vec<u8> {
        let mut bytes =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n")
                .into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    #[tokio::test]
    async fn interrupted_payload_is_retried_then_verified() {
        let body = b"verified runtime payload";
        let hash = BlobHash::of(body);
        let (url, worker) = server(vec![
            response(body.len(), b"partial"),
            response(body.len(), body),
        ]);
        let blobs = HttpBlobs::new(url).fetch_all(&[hash]).await.unwrap();
        assert_eq!(blobs, vec![Bytes::from_static(body)]);
        assert_eq!(worker.join().unwrap(), 2);
    }

    #[tokio::test]
    async fn permanent_interruption_stops_after_three_attempts_with_source_error() {
        let (url, worker) = server(vec![response(20, b"partial"); 3]);
        let hash = BlobHash::of(b"expected");
        let error = HttpBlobs::new(url)
            .fetch_all(&[hash])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("read http://"), "{error}");
        assert!(error.contains("after 3 HTTP attempt(s)"), "{error}");
        assert!(
            error.contains("body"),
            "underlying transport error missing: {error}"
        );
        assert_eq!(worker.join().unwrap(), 3);
    }

    #[tokio::test]
    async fn stalled_requests_obey_deadlines_and_exhaust_bounded_retries() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let mut held = Vec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0; 1024];
                assert!(stream.read(&mut request).unwrap() > 0);
                held.push(stream);
            }
            // Keep every response silent and open until the request deadlines
            // have expired; EOF cannot stand in for a timeout in this test.
            held
        });
        let client = HttpBlobs::client().unwrap();
        let start = std::time::Instant::now();
        let error = HttpBlobs::get_with_deadline(
            &client,
            &format!("http://{address}/stalled"),
            8,
            Duration::from_millis(200),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("after 3 HTTP attempt(s)"), "{error}");
        assert!(error.contains("timed out"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(5));
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn connection_refusal_is_retried_with_a_finite_budget() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let client = HttpBlobs::client().unwrap();
        let error = HttpBlobs::get_bounded(&client, &format!("http://{address}/missing"), 8)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("after 3 HTTP attempt(s)"), "{error}");
        assert!(
            error.to_ascii_lowercase().contains("connection refused"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn size_and_integrity_failures_are_not_retried() {
        let client = HttpBlobs::client().unwrap();
        let (url, worker) = server(vec![response(9, b"oversized")]);
        let error = HttpBlobs::get_bounded(&client, &url, 8)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("8-byte object ceiling"), "{error}");
        assert!(error.contains("after 1 HTTP attempt(s)"), "{error}");
        assert_eq!(worker.join().unwrap(), 1);

        let (url, worker) = server(vec![response(7, b"corrupt")]);
        let hash = BlobHash::of(b"correct");
        let error = HttpBlobs::new(url)
            .fetch_all(&[hash])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("do not hash to the payload blob"), "{error}");
        assert_eq!(worker.join().unwrap(), 1);
    }
}
