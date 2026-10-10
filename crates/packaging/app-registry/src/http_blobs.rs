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
//! buffer without bound.
//!
//! Every object is asked for as its compressed twin first, `<url>.zst`
//! ([`tilo_app_package::compressed`]), and taken raw when the registry has
//! no twin (404). A twin is decompressed with the object's bound — the
//! chunk's listed length, or [`MAX_OBJECT_BYTES`] for a whole blob — before
//! any of the checks above, which see only decompressed bytes: compression
//! changes how many bytes cross the network, never what is verified.
//!
//! An installer's [`BlobFetcher`] keeps [`IN_FLIGHT`] requests going while it
//! hands the sink one object: a runtime is about thirty objects, and fetched
//! one after another each costs a full round trip before its first byte. Locating
//! a chunked blob alone is three sequential requests (twin, raw, chunk list),
//! each a full round trip on a registry far away, so the window is wide enough
//! to cover a whole runtime in a few waves.
//! Delivery stays in order. Memory is bounded by objects, not by the
//! package: at most [`IN_FLIGHT`] blobs being located ahead, plus, while a
//! chunked blob is delivered, [`IN_FLIGHT`] of its chunks ahead, plus the
//! object being delivered — each at most [`MAX_OBJECT_BYTES`], in practice
//! at most [`tilo_app_package::chunks::CHUNK_BYTES`].

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;
use tilo_app_package::chunks::{
    ChunkList, ChunkRef, ChunkedReader, MAX_CHUNK_LIST_BYTES, MAX_OBJECT_BYTES, Reassembly,
};
use tilo_app_package::compressed;
use tilo_app_package::{BlobFetcher, BlobHash, BlobSink, PackageFetcher, PackageId};

/// Requests an installer keeps going ahead of the object it is delivering.
pub const IN_FLIGHT: usize = 8;

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

    /// The object at `url`, at most `limit` bytes: its compressed twin
    /// decompressed within `limit`, or, when the registry keeps no twin, the
    /// raw object. `None` when it has neither. The caller verifies the bytes.
    async fn get_object(client: &reqwest::Client, url: &str, limit: u64) -> Result<Option<Bytes>> {
        let twin_url = format!("{url}{}", compressed::SUFFIX);
        let Some(twin) =
            Self::get_bounded(client, &twin_url, compressed::compressed_bound(limit)).await?
        else {
            return Self::get_bounded(client, url, limit).await;
        };
        // Decompressing a large chunk is CPU work; keep it off the task that
        // drives the other requests in flight.
        let bytes = tokio::task::spawn_blocking(move || {
            let mut bytes = Vec::new();
            compressed::decompress_bounded(twin.as_ref(), limit, &mut bytes).map(|_| bytes)
        })
        .await
        .context("decompress task")?
        .map_err(|error| anyhow!("{twin_url}: {error}"))?;
        Ok(Some(Bytes::from(bytes)))
    }

    /// The whole blob `hash`, or its chunk list when the registry keeps it
    /// as chunks.
    async fn locate(&self, client: &reqwest::Client, hash: &BlobHash) -> Result<Stored> {
        let url = format!("{}/blobs/{hash}", self.base_url);
        if let Some(body) = Self::get_object(client, &url, MAX_OBJECT_BYTES).await? {
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
        Self::get_object(client, &url, chunk.len.min(MAX_OBJECT_BYTES))
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

    /// Deliver one located blob to `sink`: whole, or chunk by verified
    /// chunk with up to [`IN_FLIGHT`] of the following chunks requested.
    fn deliver(
        &self,
        runtime: &tokio::runtime::Runtime,
        client: &reqwest::Client,
        hash: &BlobHash,
        stored: Stored,
        sink: &mut dyn BlobSink,
    ) -> std::io::Result<()> {
        match stored {
            Stored::Whole(bytes) => sink.accept(hash, bytes.len() as u64, &mut bytes.as_ref()),
            Stored::Chunked(list) => {
                let len = list.len;
                let mut ahead = Ahead::new(
                    runtime,
                    list.chunks.clone().into_iter().map(|chunk| {
                        let blobs = self.clone();
                        let client = client.clone();
                        async move {
                            let bytes = blobs.fetch_chunk(&client, &chunk).await?;
                            Ok((chunk.hash, bytes))
                        }
                    }),
                );
                let mut reader = ChunkedReader::new(list, |chunk: &ChunkRef| {
                    let (fetched, bytes) = ahead.next().ok_or_else(|| {
                        std::io::Error::other(format!("payload blob {hash} has no chunk left"))
                    })??;
                    // The window runs in list order; a mismatch is a bug here,
                    // and Reassembly would refuse the bytes anyway.
                    if fetched != chunk.hash {
                        return Err(std::io::Error::other(format!(
                            "chunk {} arrived where {} was expected",
                            fetched, chunk.hash
                        )));
                    }
                    Ok(bytes.to_vec())
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
        let mut located = Ahead::new(
            &runtime,
            hashes.iter().copied().map(|hash| {
                let blobs = self.clone();
                let client = client.clone();
                async move { blobs.locate(&client, &hash).await }
            }),
        );
        for hash in hashes {
            let stored = located.next().ok_or_else(|| {
                std::io::Error::other(format!("payload blob {hash} was not requested"))
            })??;
            self.deliver(&runtime, &client, hash, stored, sink)?;
        }
        Ok(())
    }
}

/// Requests started [`IN_FLIGHT`] ahead of the one being consumed, answered
/// in the order they were made. Each runs as a task on `runtime`, so the
/// ones ahead make progress whenever the consumer waits for the next.
struct Ahead<'r, I, F>
where
    I: Iterator<Item = F>,
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime: &'r tokio::runtime::Runtime,
    pending: I,
    running: VecDeque<tokio::task::JoinHandle<F::Output>>,
}

impl<'r, I, F, T> Ahead<'r, I, F>
where
    I: Iterator<Item = F>,
    F: Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    fn new(runtime: &'r tokio::runtime::Runtime, pending: I) -> Self {
        Self {
            runtime,
            pending,
            running: VecDeque::with_capacity(IN_FLIGHT.saturating_add(1)),
        }
    }

    /// The next answer in request order, or `None` after the last one. The
    /// request after the window is started before waiting, so [`IN_FLIGHT`]
    /// stay in flight while the caller handles this one.
    fn next(&mut self) -> Option<std::io::Result<T>> {
        while self.running.len() <= IN_FLIGHT {
            let Some(request) = self.pending.next() else {
                break;
            };
            self.running.push_back(self.runtime.spawn(request));
        }
        let handle = self.running.pop_front()?;
        Some(match self.runtime.block_on(handle) {
            Ok(answer) => answer.map_err(std::io::Error::other),
            Err(join) => Err(std::io::Error::other(join)),
        })
    }
}

impl<I, F> Drop for Ahead<'_, I, F>
where
    I: Iterator<Item = F>,
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn drop(&mut self) {
        for handle in &self.running {
            handle.abort();
        }
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

    /// The answer of a registry that keeps no compressed twin.
    fn not_found() -> Vec<u8> {
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
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
            not_found(),
            response(body.len(), b"partial"),
            response(body.len(), body),
        ]);
        let blobs = HttpBlobs::new(url).fetch_all(&[hash]).await.unwrap();
        assert_eq!(blobs, vec![Bytes::from_static(body)]);
        assert_eq!(worker.join().unwrap(), 3);
    }

    #[tokio::test]
    async fn permanent_interruption_stops_after_three_attempts_with_source_error() {
        let mut responses = vec![not_found()];
        responses.extend(vec![response(20, b"partial"); 3]);
        let (url, worker) = server(responses);
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
        assert_eq!(worker.join().unwrap(), 4);
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

        let (url, worker) = server(vec![not_found(), response(7, b"corrupt")]);
        let hash = BlobHash::of(b"correct");
        let error = HttpBlobs::new(url)
            .fetch_all(&[hash])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("do not hash to the payload blob"), "{error}");
        assert_eq!(worker.join().unwrap(), 2);
    }

    /// A server answering every request on its own thread after `delay`:
    /// `objects` maps a path to its body, anything else is a 404.
    fn slow_server(objects: std::collections::HashMap<String, Vec<u8>>, delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let objects = objects.clone();
                std::thread::spawn(move || {
                    let mut request = [0; 2048];
                    let read = stream.read(&mut request).unwrap_or(0);
                    let text = String::from_utf8_lossy(&request[..read]).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                    std::thread::sleep(delay);
                    let reply = match objects.get(&path) {
                        Some(body) => response(body.len(), body),
                        None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec(),
                    };
                    let _ = stream.write_all(&reply);
                });
            }
        });
        format!("http://{address}")
    }

    fn twin(bytes: &[u8]) -> Vec<u8> {
        compressed::compress(bytes).unwrap()
    }

    /// Collects delivered blobs in order.
    #[derive(Default)]
    struct Collected(Vec<(BlobHash, Vec<u8>)>);

    impl BlobSink for Collected {
        fn accept(
            &mut self,
            hash: &BlobHash,
            len: u64,
            bytes: &mut dyn std::io::Read,
        ) -> std::io::Result<()> {
            let mut body = Vec::new();
            bytes.take(len).read_to_end(&mut body)?;
            self.0.push((*hash, body));
            Ok(())
        }
    }

    #[test]
    fn fetch_overlaps_requests() {
        let bodies: Vec<Vec<u8>> = (0..8u8).map(|n| vec![n; 64]).collect();
        let hashes: Vec<BlobHash> = bodies.iter().map(|body| BlobHash::of(body)).collect();
        // Twins, so each blob is one request as from a registry that has them.
        let objects = hashes
            .iter()
            .zip(&bodies)
            .map(|(hash, body)| (format!("/blobs/{hash}.zst"), twin(body)))
            .collect();
        let delay = Duration::from_millis(200);
        let url = slow_server(objects, delay);
        let mut sink = Collected::default();
        let started = std::time::Instant::now();
        HttpBlobs::new(url).fetch(&hashes, &mut sink).unwrap();
        let elapsed = started.elapsed();
        // One at a time this is eight delays; with requests overlapped it is
        // two windows.
        assert!(elapsed < delay * 5, "8 blobs took {elapsed:?}");
        let delivered: Vec<_> = sink.0.iter().map(|(hash, _)| *hash).collect();
        assert_eq!(delivered, hashes, "delivered in request order");
        for ((_, body), expected) in sink.0.iter().zip(&bodies) {
            assert_eq!(body, expected);
        }
    }

    /// A server that answers after `delay` and records the most requests it
    /// ever had open at once.
    fn counting_server(
        objects: std::collections::HashMap<String, Vec<u8>>,
        delay: Duration,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let peak = std::sync::Arc::new(AtomicUsize::new(0));
        let open = std::sync::Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let seen = peak.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let (objects, open, peak) = (objects.clone(), open.clone(), seen.clone());
                std::thread::spawn(move || {
                    let mut request = [0; 2048];
                    let read = stream.read(&mut request).unwrap_or(0);
                    let text = String::from_utf8_lossy(&request[..read]).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                    peak.fetch_max(open.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    std::thread::sleep(delay);
                    open.fetch_sub(1, Ordering::SeqCst);
                    let reply = match objects.get(&path) {
                        Some(body) => response(body.len(), body),
                        None => not_found(),
                    };
                    let _ = stream.write_all(&reply);
                });
            }
        });
        (format!("http://{address}"), peak)
    }

    #[test]
    fn requests_in_flight_are_bounded_by_the_window() {
        let bodies: Vec<Vec<u8>> = (0..40u8).map(|n| vec![n; 64]).collect();
        let hashes: Vec<BlobHash> = bodies.iter().map(|body| BlobHash::of(body)).collect();
        let objects = hashes
            .iter()
            .zip(&bodies)
            .map(|(hash, body)| (format!("/blobs/{hash}.zst"), twin(body)))
            .collect();
        let (url, peak) = counting_server(objects, Duration::from_millis(100));
        let mut sink = Collected::default();
        HttpBlobs::new(url).fetch(&hashes, &mut sink).unwrap();
        let peak = peak.load(std::sync::atomic::Ordering::SeqCst);
        // The object being delivered plus the window ahead of it.
        assert!(peak <= IN_FLIGHT + 1, "peak {peak} exceeds the window");
        assert!(peak >= IN_FLIGHT, "peak {peak}: the window was not used");
        assert_eq!(sink.0.len(), hashes.len());
    }

    #[test]
    fn chunked_blobs_arrive_in_order_and_verified() {
        let blob = b"chunked payload, fetched ahead".to_vec();
        let hash = BlobHash::of(&blob);
        let chunk_bytes = 4u64;
        let chunks: Vec<ChunkRef> = blob
            .chunks(4)
            .map(|piece| ChunkRef {
                hash: BlobHash::of(piece),
                len: piece.len() as u64,
            })
            .collect();
        let list = ChunkList::new(hash, blob.len() as u64, chunk_bytes, chunks);
        let mut objects: std::collections::HashMap<String, Vec<u8>> = blob
            .chunks(4)
            .map(|piece| (format!("/blobs/{}", BlobHash::of(piece)), piece.to_vec()))
            .collect();
        objects.insert(format!("/blobs/{hash}.chunks"), list.encode());
        let small = b"a whole blob after it".to_vec();
        objects.insert(format!("/blobs/{}", BlobHash::of(&small)), small.clone());
        let url = slow_server(objects.clone(), Duration::from_millis(20));
        let mut sink = Collected::default();
        HttpBlobs::new(&url)
            .fetch(&[hash, BlobHash::of(&small)], &mut sink)
            .unwrap();
        assert_eq!(
            sink.0,
            vec![(hash, blob.clone()), (BlobHash::of(&small), small)]
        );

        // A chunk the server corrupts stops the blob before its last byte.
        let first = BlobHash::of(&blob[..4]);
        objects.insert(format!("/blobs/{first}"), b"evil".to_vec());
        let url = slow_server(objects, Duration::from_millis(1));
        let mut sink = Collected::default();
        assert!(HttpBlobs::new(url).fetch(&[hash], &mut sink).is_err());
        assert!(sink.0.iter().all(|(_, body)| body != &blob));
    }
    #[test]
    fn twins_are_preferred_and_raw_objects_are_the_fallback() {
        let whole = b"a whole runtime library, compressed for the wire ".repeat(64);
        let raw_only = b"an object stored before twins existed".to_vec();
        let (whole_hash, raw_hash) = (BlobHash::of(&whole), BlobHash::of(&raw_only));
        let objects = std::collections::HashMap::from([
            // The raw copy is wrong: delivering the right bytes proves the
            // twin was taken and verified after decompression.
            (format!("/blobs/{whole_hash}"), b"not it".to_vec()),
            (format!("/blobs/{whole_hash}.zst"), twin(&whole)),
            // No twin: 404, then the raw object.
            (format!("/blobs/{raw_hash}"), raw_only.clone()),
        ]);
        let url = slow_server(objects, Duration::from_millis(1));
        let mut sink = Collected::default();
        HttpBlobs::new(url)
            .fetch(&[whole_hash, raw_hash], &mut sink)
            .unwrap();
        assert_eq!(sink.0, vec![(whole_hash, whole), (raw_hash, raw_only)]);
    }

    #[test]
    fn a_twin_is_verified_after_decompression() {
        let body = b"the payload the signed collection names".to_vec();
        let hash = BlobHash::of(&body);
        let objects = std::collections::HashMap::from([
            (format!("/blobs/{hash}"), body),
            (
                format!("/blobs/{hash}.zst"),
                twin(b"a well-formed twin of other bytes"),
            ),
        ]);
        let url = slow_server(objects, Duration::from_millis(1));
        let error = HttpBlobs::new(url)
            .fetch(&[hash], &mut Collected::default())
            .unwrap_err()
            .to_string();
        assert!(error.contains("do not hash to the payload blob"), "{error}");
    }

    #[test]
    fn a_chunk_twin_that_expands_past_its_listed_length_is_refused() {
        let blob = b"chunked payload whose second twin is a bomb".to_vec();
        let hash = BlobHash::of(&blob);
        let pieces: Vec<&[u8]> = blob.chunks(16).collect();
        let chunks: Vec<ChunkRef> = pieces
            .iter()
            .map(|piece| ChunkRef {
                hash: BlobHash::of(piece),
                len: piece.len() as u64,
            })
            .collect();
        let list = ChunkList::new(hash, blob.len() as u64, 16, chunks.clone());
        let mut objects: std::collections::HashMap<String, Vec<u8>> = pieces
            .iter()
            .map(|piece| (format!("/blobs/{}.zst", BlobHash::of(piece)), twin(piece)))
            .collect();
        objects.insert(format!("/blobs/{hash}.chunks"), list.encode());
        let url = slow_server(objects.clone(), Duration::from_millis(1));
        let mut sink = Collected::default();
        HttpBlobs::new(&url).fetch(&[hash], &mut sink).unwrap();
        assert_eq!(sink.0, vec![(hash, blob.clone())]);

        // Sixteen listed bytes, a megabyte of zeros behind them.
        objects.insert(
            format!("/blobs/{}.zst", chunks[1].hash),
            twin(&vec![0; 1 << 20]),
        );
        let url = slow_server(objects, Duration::from_millis(1));
        let mut sink = Collected::default();
        let error = HttpBlobs::new(url)
            .fetch(&[hash], &mut sink)
            .unwrap_err()
            .to_string();
        assert!(error.contains("expands past its 16-byte bound"), "{error}");
        assert!(sink.0.iter().all(|(_, body)| body != &blob));
    }
}
