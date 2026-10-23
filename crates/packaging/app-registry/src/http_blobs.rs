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
//! A blob is located in one round trip: its twin and its chunk list are asked
//! for together, since the registry keeps one or the other. Asked in turn
//! (twin, raw, chunk list), a chunked blob cost three full round trips to a
//! registry far away before its first chunk was requested.
//!
//! An installer's [`BlobFetcher`] keeps [`IN_FLIGHT`] blobs being located and
//! [`IN_FLIGHT`] chunks being fetched while it hands the sink one object: a
//! runtime is about thirty objects, and fetched one after another each costs
//! a full round trip before its first byte. The chunks form one stream across
//! blobs, so the next blob's first chunks are already on their way while the
//! last chunks of this one arrive, instead of a round trip's pause between
//! every two chunked blobs.
//! Delivery stays in order. Memory is bounded by objects, not by the
//! package: at most [`IN_FLIGHT`] blobs located ahead and as many more being
//! located, [`IN_FLIGHT`] chunks ahead, and the object being delivered — each
//! at most [`MAX_OBJECT_BYTES`], in practice at most
//! [`tilo_app_package::chunks::CHUNK_BYTES`].

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;
use tilo_app_package::chunks::{
    CHUNKS_SUFFIX, ChunkList, ChunkRef, ChunkedReader, MAX_CHUNK_LIST_BYTES, MAX_OBJECT_BYTES,
    Reassembly,
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
        Self::decompress(twin, limit, &twin_url).await.map(Some)
    }

    /// The object a twin fetched from `twin_url` stands for, at most `limit`
    /// bytes. The caller verifies them.
    async fn decompress(twin: Bytes, limit: u64, twin_url: &str) -> Result<Bytes> {
        // Decompressing a large chunk is CPU work; keep it off the task that
        // drives the other requests in flight.
        let bytes = tokio::task::spawn_blocking(move || {
            let mut bytes = Vec::new();
            compressed::decompress_bounded(twin.as_ref(), limit, &mut bytes).map(|_| bytes)
        })
        .await
        .context("decompress task")?
        .map_err(|error| anyhow!("{twin_url}: {error}"))?;
        Ok(Bytes::from(bytes))
    }

    /// The whole blob `hash`, or its chunk list when the registry keeps it
    /// as chunks.
    ///
    /// The registry keeps a blob whole, with a compressed twin beside it, or
    /// as chunks with a chunk list, so the twin and the list are asked for at
    /// once and one of them answers: one round trip either way. Only an
    /// object stored before twins existed takes a second, for its raw bytes.
    async fn locate(&self, client: &reqwest::Client, hash: &BlobHash) -> Result<Stored> {
        let url = format!("{}/blobs/{hash}", self.base_url);
        let twin_url = format!("{url}{}", compressed::SUFFIX);
        let list_url = format!("{url}{CHUNKS_SUFFIX}");
        let (twin, list) = tokio::join!(
            Self::get_bounded(
                client,
                &twin_url,
                compressed::compressed_bound(MAX_OBJECT_BYTES)
            ),
            Self::get_bounded(client, &list_url, MAX_CHUNK_LIST_BYTES),
        );
        let body = match twin? {
            Some(twin) => Some(Self::decompress(twin, MAX_OBJECT_BYTES, &twin_url).await?),
            None => match list? {
                Some(list) => {
                    let list = ChunkList::decode(&list, hash)
                        .with_context(|| format!("decode {list_url}"))?;
                    return Ok(Stored::Chunked(list));
                }
                None => Self::get_bounded(client, &url, MAX_OBJECT_BYTES).await?,
            },
        };
        let Some(body) = body else {
            bail!(
                "registry {} has payload blob {hash} neither whole nor as chunks",
                self.base_url
            );
        };
        if blake3::hash(&body).as_bytes() != hash.as_bytes() {
            return Err(anyhow!(
                "{url} served bytes that do not hash to the payload blob"
            ));
        }
        Ok(Stored::Whole(body))
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
        let feed = RefCell::new(Feed {
            located: Ahead::new(
                &runtime,
                hashes.iter().copied().map(|hash| {
                    let blobs = self.clone();
                    let client = client.clone();
                    async move { blobs.locate(&client, &hash).await }
                }),
            ),
            blobs: VecDeque::new(),
            chunks: VecDeque::new(),
        });
        // One window over the chunks of every blob, in delivery order. It
        // stops taking blobs off the locate window while IN_FLIGHT located
        // blobs wait for delivery, so a long run of whole blobs is not held
        // in memory to find the next chunk.
        let mut chunks = Ahead::new(
            &runtime,
            std::iter::from_fn(|| {
                let mut feed = feed.borrow_mut();
                loop {
                    if let Some(chunk) = feed.chunks.pop_front() {
                        let blobs = self.clone();
                        let client = client.clone();
                        return Some(async move {
                            let bytes = blobs.fetch_chunk(&client, &chunk).await?;
                            Ok((chunk.hash, bytes))
                        });
                    }
                    if feed.blobs.len() > IN_FLIGHT || !feed.pull() {
                        return None;
                    }
                }
            }),
        );
        for hash in hashes {
            let stored = {
                let mut feed = feed.borrow_mut();
                loop {
                    if let Some(stored) = feed.blobs.pop_front() {
                        break stored;
                    }
                    if !feed.pull() {
                        break Err(std::io::Error::other(format!(
                            "payload blob {hash} was not requested"
                        )));
                    }
                }
            }?;
            match stored {
                Stored::Whole(bytes) => {
                    sink.accept(hash, bytes.len() as u64, &mut bytes.as_ref())?;
                }
                Stored::Chunked(list) => {
                    let len = list.len;
                    let mut reader = ChunkedReader::new(list, |chunk: &ChunkRef| {
                        let (fetched, bytes) = chunks.next().ok_or_else(|| {
                            std::io::Error::other(format!("payload blob {hash} has no chunk left"))
                        })??;
                        // The window runs in list order; a mismatch is a bug
                        // here, and Reassembly would refuse the bytes anyway.
                        if fetched != chunk.hash {
                            return Err(std::io::Error::other(format!(
                                "chunk {} arrived where {} was expected",
                                fetched, chunk.hash
                            )));
                        }
                        Ok(bytes.to_vec())
                    });
                    sink.accept(hash, len, &mut reader)?;
                    // A sink that stopped early still gets no unverified
                    // blob: finish verifying what it did not read.
                    std::io::copy(&mut reader, &mut std::io::sink())?;
                    if !reader.is_complete() {
                        return Err(std::io::Error::other(format!(
                            "payload blob {hash} did not complete"
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

/// The blobs [`BlobFetcher::fetch`] has taken off its locate window and not
/// delivered yet, and the chunks of those that are chunked, not yet
/// requested.
struct Feed<'r, I, F>
where
    I: Iterator<Item = F>,
    F: Future<Output = Result<Stored>> + Send + 'static,
{
    located: Ahead<'r, I, F>,
    blobs: VecDeque<std::io::Result<Stored>>,
    chunks: VecDeque<ChunkRef>,
}

impl<I, F> Feed<'_, I, F>
where
    I: Iterator<Item = F>,
    F: Future<Output = Result<Stored>> + Send + 'static,
{
    /// Take the next located blob, queueing its chunks; `false` after the
    /// last.
    fn pull(&mut self) -> bool {
        let Some(located) = self.located.next() else {
            return false;
        };
        if let Ok(Stored::Chunked(list)) = &located {
            self.chunks.extend(list.chunks.iter().copied());
        }
        self.blobs.push_back(located);
        true
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
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
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

    /// A server answering each path with its scripted responses in turn
    /// (404 once a path's script is used up), on a thread per connection;
    /// returns its URL and how many requests it has answered.
    fn scripted(
        script: Vec<(String, Vec<Vec<u8>>)>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let script: std::collections::HashMap<String, VecDeque<Vec<u8>>> = script
            .into_iter()
            .map(|(path, responses)| (path, responses.into()))
            .collect();
        let script = std::sync::Arc::new(std::sync::Mutex::new(script));
        let answered = std::sync::Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let count = answered.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let (script, count) = (script.clone(), count.clone());
                std::thread::spawn(move || {
                    let mut request = [0; 2048];
                    let read = stream.read(&mut request).unwrap_or(0);
                    let text = String::from_utf8_lossy(&request[..read]).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                    let reply = script
                        .lock()
                        .unwrap()
                        .get_mut(&path)
                        .and_then(VecDeque::pop_front)
                        .unwrap_or_else(not_found);
                    count.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(&reply);
                });
            }
        });
        (format!("http://{address}"), answered)
    }

    /// The twin and chunk-list requests of a blob the registry keeps raw,
    /// from before twins existed, followed by its raw `responses`.
    fn raw_only(hash: &BlobHash, responses: Vec<Vec<u8>>) -> Vec<(String, Vec<Vec<u8>>)> {
        vec![
            (format!("/blobs/{hash}.zst"), vec![not_found()]),
            (format!("/blobs/{hash}.chunks"), vec![not_found()]),
            (format!("/blobs/{hash}"), responses),
        ]
    }

    #[tokio::test]
    async fn interrupted_payload_is_retried_then_verified() {
        let body = b"verified runtime payload";
        let hash = BlobHash::of(body);
        let (url, answered) = scripted(raw_only(
            &hash,
            vec![response(body.len(), b"partial"), response(body.len(), body)],
        ));
        let blobs = HttpBlobs::new(url).fetch_all(&[hash]).await.unwrap();
        assert_eq!(blobs, vec![Bytes::from_static(body)]);
        assert_eq!(answered.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn permanent_interruption_stops_after_three_attempts_with_source_error() {
        let hash = BlobHash::of(b"expected");
        let (url, answered) = scripted(raw_only(&hash, vec![response(20, b"partial"); 3]));
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
        assert_eq!(answered.load(std::sync::atomic::Ordering::SeqCst), 5);
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
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
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
        // The server never answers: only the deadlines end the call.
        let error = tilo_test_env::within(
            "the stalled requests to time out",
            HttpBlobs::get_with_deadline(
                &client,
                &format!("http://{address}/stalled"),
                8,
                Duration::from_millis(200),
            ),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("after 3 HTTP attempt(s)"), "{error}");
        assert!(error.contains("timed out"), "{error}");
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

        let hash = BlobHash::of(b"correct");
        let (url, answered) = scripted(raw_only(&hash, vec![response(7, b"corrupt")]));
        let error = HttpBlobs::new(url)
            .fetch_all(&[hash])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("do not hash to the payload blob"), "{error}");
        assert_eq!(answered.load(std::sync::atomic::Ordering::SeqCst), 3);
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
        // The server holds every answer until a window of requests is open
        // at once, which a fetch asking one at a time never reaches.
        let (url, peak) = counting_server(objects, IN_FLIGHT);
        let mut sink = Collected::default();
        HttpBlobs::new(url).fetch(&hashes, &mut sink).unwrap();
        let peak = peak.load(std::sync::atomic::Ordering::SeqCst);
        assert!(peak >= IN_FLIGHT, "only {peak} requests overlapped");
        let delivered: Vec<_> = sink.0.iter().map(|(hash, _)| *hash).collect();
        assert_eq!(delivered, hashes, "delivered in request order");
        for ((_, body), expected) in sink.0.iter().zip(&bodies) {
            assert_eq!(body, expected);
        }
    }

    /// A server that records the most requests it ever had open at once,
    /// and holds every answer until that reaches `together` (or the hang
    /// bound runs out): requests asked one at a time can never overlap by
    /// chance, and requests asked together always do.
    fn counting_server(
        objects: std::collections::HashMap<String, Vec<u8>>,
        together: usize,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let peak = std::sync::Arc::new(AtomicUsize::new(0));
        let open = std::sync::Arc::new(AtomicUsize::new(0));
        let reached = std::sync::Arc::new((std::sync::Mutex::new(()), std::sync::Condvar::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let seen = peak.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let (objects, open, peak) = (objects.clone(), open.clone(), seen.clone());
                let reached = reached.clone();
                std::thread::spawn(move || {
                    let mut request = [0; 2048];
                    let read = stream.read(&mut request).unwrap_or(0);
                    let text = String::from_utf8_lossy(&request[..read]).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                    let (lock, changed) = &*reached;
                    let now_open = open.fetch_add(1, Ordering::SeqCst).saturating_add(1);
                    peak.fetch_max(now_open, Ordering::SeqCst);
                    // Through the lock, so a waiter either sees the new peak
                    // or is already waiting for this notice.
                    drop(lock.lock().unwrap());
                    changed.notify_all();
                    drop(
                        changed
                            .wait_timeout_while(
                                lock.lock().unwrap(),
                                tilo_test_env::hang_bound(),
                                |()| peak.load(Ordering::SeqCst) < together,
                            )
                            .unwrap(),
                    );
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
        let (url, peak) = counting_server(objects, IN_FLIGHT);
        let mut sink = Collected::default();
        HttpBlobs::new(url).fetch(&hashes, &mut sink).unwrap();
        let peak = peak.load(std::sync::atomic::Ordering::SeqCst);
        // The object being delivered plus the window ahead of it, each
        // located by asking for its twin and its chunk list at once.
        assert!(
            peak <= 2 * (IN_FLIGHT + 1),
            "peak {peak} exceeds the window"
        );
        assert!(peak >= IN_FLIGHT, "peak {peak}: the window was not used");
        assert_eq!(sink.0.len(), hashes.len());
    }

    /// A runtime-shaped payload — large libraries kept as chunks between
    /// small whole files: the objects a registry serves for it, the blobs to
    /// fetch, and what each delivers.
    #[allow(clippy::type_complexity, reason = "a test fixture's three parts")]
    fn chunked_payload() -> (
        std::collections::HashMap<String, Vec<u8>>,
        Vec<BlobHash>,
        Vec<(BlobHash, Vec<u8>)>,
    ) {
        let mut objects = std::collections::HashMap::new();
        let mut hashes = Vec::new();
        let mut expected = Vec::new();
        for blob in 0..3_u8 {
            let bytes: Vec<u8> = (0..60_u32)
                .map(|index| u8::try_from(index % 251).unwrap() ^ blob)
                .collect();
            let hash = BlobHash::of(&bytes);
            let pieces: Vec<&[u8]> = bytes.chunks(10).collect();
            let chunks = pieces
                .iter()
                .map(|piece| ChunkRef {
                    hash: BlobHash::of(piece),
                    len: piece.len() as u64,
                })
                .collect();
            for piece in &pieces {
                objects.insert(format!("/blobs/{}.zst", BlobHash::of(piece)), twin(piece));
            }
            objects.insert(
                format!("/blobs/{hash}.chunks"),
                ChunkList::new(hash, bytes.len() as u64, 10, chunks).encode(),
            );
            hashes.push(hash);
            expected.push((hash, bytes));
            let small = vec![blob; 32];
            objects.insert(format!("/blobs/{}.zst", BlobHash::of(&small)), twin(&small));
            hashes.push(BlobHash::of(&small));
            expected.push((BlobHash::of(&small), small));
        }
        (objects, hashes, expected)
    }

    #[test]
    fn a_chunked_payload_arrives_whole_and_in_order() {
        let (objects, hashes, expected) = chunked_payload();
        let url = slow_server(objects, Duration::ZERO);
        let mut sink = Collected::default();
        HttpBlobs::new(url).fetch(&hashes, &mut sink).unwrap();
        assert_eq!(sink.0, expected);
    }

    /// The payload from a registry a round trip away. Each chunked blob is
    /// located in one round trip, and its chunks follow the previous blob's
    /// without a pause, so the whole payload costs about one round trip to
    /// locate and one per window of chunks. Located in three round trips and
    /// fetched blob by blob, the same payload took twice as long.
    #[test]
    #[ignore = "measurement; run with --ignored --nocapture"]
    fn a_chunked_payload_streams_without_a_round_trip_per_blob() {
        const ROUND_TRIP: Duration = Duration::from_millis(150);
        let (objects, hashes, expected) = chunked_payload();
        let url = slow_server(objects, ROUND_TRIP);
        let mut sink = Collected::default();
        let started = std::time::Instant::now();
        HttpBlobs::new(url).fetch(&hashes, &mut sink).unwrap();
        let elapsed = started.elapsed();
        eprintln!(
            "perf 3 chunked blobs of 6 chunks and 3 whole blobs, {ROUND_TRIP:?} round trip: {elapsed:?}"
        );
        assert_eq!(sink.0, expected);
        // Locating, then 18 chunks in windows of IN_FLIGHT + 1.
        assert!(
            elapsed < ROUND_TRIP * 4,
            "the payload took {elapsed:?}, {:.1} round trips",
            elapsed.as_secs_f64() / ROUND_TRIP.as_secs_f64()
        );
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
