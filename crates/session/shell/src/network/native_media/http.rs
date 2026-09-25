//! Bounded transport for exact actor-prepared media operations. Authority and
//! signing remain in the actor; native TLS trust is independently required.
use super::protocol::PreparedRequest;
use std::time::Duration;
use tilo_relay::delivery_v3::{descriptor, http_proof, media_wire};
const DEADLINE: Duration = Duration::from_secs(3);
#[derive(Clone)]
pub(super) struct Http {
    client: reqwest::Client,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Error {
    Invalid,
    Unavailable,
    Timeout,
    Status,
    Capacity,
}
impl Http {
    pub fn new() -> Result<Self, Error> {
        let client = Self::builder().build().map_err(|_| Error::Unavailable)?;
        Ok(Self { client })
    }
    fn builder() -> reqwest::ClientBuilder {
        reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(DEADLINE)
            .timeout(DEADLINE)
            .pool_max_idle_per_host(1)
            .pool_idle_timeout(Duration::from_secs(5))
    }
    pub async fn send(&self, request: &PreparedRequest) -> Result<Vec<u8>, Error> {
        self.send_for(request, DEADLINE).await
    }
    async fn send_for(&self, request: &PreparedRequest, limit: Duration) -> Result<Vec<u8>, Error> {
        let (get, maximum) = validate(request)?;
        let operation = async {
            let url = format!("{}{}", request.origin, request.target);
            let mut builder = if get {
                self.client.get(url)
            } else {
                self.client
                    .post(url)
                    .header("content-type", "application/octet-stream")
                    .body(request.body.clone())
            };
            if let Some(proof) = &request.proof {
                builder = builder.header("x-tilo-delivery-proof", hex::encode(proof));
            }
            let mut response = builder.send().await.map_err(|_| Error::Unavailable)?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(Error::Status);
            }
            if response
                .content_length()
                .is_some_and(|n| n > maximum as u64)
            {
                return Err(Error::Capacity);
            }
            // A fixed, already bounded destination prevents Vec growth from
            // reserving beyond the wire limit for an unknown-length response.
            let mut result = Vec::with_capacity(maximum);
            while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
                if chunk.len() > maximum - result.len() {
                    return Err(Error::Capacity);
                }
                result.extend_from_slice(&chunk);
            }
            if result.is_empty() {
                return Err(Error::Invalid);
            }
            Ok(result)
        };
        tokio::time::timeout(limit, operation)
            .await
            .map_err(|_| Error::Timeout)?
    }
}
fn validate(request: &PreparedRequest) -> Result<(bool, usize), Error> {
    if request.origin.len() > 2048 || !request.origin.is_ascii() {
        return Err(Error::Invalid);
    }
    let origin = reqwest::Url::parse(&request.origin).map_err(|_| Error::Invalid)?;
    if origin.scheme() != "https"
        || origin.origin().ascii_serialization() != request.origin
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(Error::Invalid);
    }
    let (get, input, output, proof) = match request.target {
        "/peer/v3/descriptor" => (true, 0, descriptor::MAX_HINT_BYTES, false),
        "/peer/v3/bootstrap" => (
            false,
            media_wire::MAX_BOOTSTRAP_REQUEST_BYTES,
            media_wire::MAX_BOOTSTRAP_RESPONSE_BYTES,
            false,
        ),
        "/peer/v3/challenge" | "/peer/v3/sessions" => (
            false,
            media_wire::MAX_CONTROL_BYTES,
            media_wire::MAX_CONTROL_BYTES,
            false,
        ),
        "/peer/v3/media/grant/challenge"
        | "/peer/v3/media/grant/complete"
        | "/peer/v3/media/relay/challenge"
        | "/peer/v3/media/relay/refresh/challenge"
        | "/peer/v3/media/relay/refresh/complete"
        | "/peer/v3/media/relay/close"
        | "/peer/v3/media/presence" => (
            false,
            media_wire::MAX_CONTROL_BYTES,
            media_wire::MAX_CONTROL_BYTES,
            true,
        ),
        _ => return Err(Error::Invalid),
    };
    if request.body.len() > input
        || (!get && request.body.is_empty())
        || request.proof.is_some() != proof
        || request
            .proof
            .as_ref()
            .is_some_and(|p| p.is_empty() || p.len() > http_proof::MAX_PROOF_BYTES)
    {
        return Err(Error::Invalid);
    }
    Ok((get, output))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(target: &'static str) -> PreparedRequest {
        PreparedRequest {
            id: 1,
            origin: "https://example.test".into(),
            target,
            body: vec![],
            proof: None,
        }
    }
    #[test]
    fn descriptor_and_bootstrap_have_distinct_real_wire_bounds() {
        let descriptor = request("/peer/v3/descriptor");
        assert_eq!(validate(&descriptor), Ok((true, 24 * 1024)));
        let mut bootstrap = request("/peer/v3/bootstrap");
        bootstrap.body = vec![1; 4096];
        assert_eq!(validate(&bootstrap), Ok((false, 32 * 1024)));
        bootstrap.body.push(1);
        assert_eq!(validate(&bootstrap), Err(Error::Invalid));
        for origin in [
            "http://example.test",
            "https://example.test/",
            "https://user@example.test",
            "https://EXAMPLE.test",
            "https://example.test:443",
        ] {
            bootstrap.origin = origin.into();
            bootstrap.body = vec![1];
            assert_eq!(validate(&bootstrap), Err(Error::Invalid));
        }
    }
    #[tokio::test]
    async fn unsigned_media_or_aliased_target_never_dials() {
        let http = Http::new().unwrap();
        for target in [
            "/peer/v3/media/grant/challenge",
            "/peer/v3/descriptor?x=1",
            "/peer/v3/descriptor/",
            "/peer/v3/../descriptor",
        ] {
            let mut req = request(target);
            req.body = vec![1];
            assert_eq!(http.send(&req).await, Err(Error::Invalid));
        }
    }
}

#[cfg(test)]
mod tls_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::Arc,
        thread,
        time::Instant,
    };
    struct Server(Option<thread::JoinHandle<()>>);
    impl Drop for Server {
        fn drop(&mut self) {
            if let Some(thread) = self.0.take() {
                thread.join().unwrap();
            }
        }
    }
    fn server(parts: Vec<(Duration, Vec<u8>)>) -> (Http, String, Server) {
        let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let client = Http::builder()
            .add_root_certificate(reqwest::Certificate::from_der(certificate.cert.der()).unwrap())
            .build()
            .unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der())
                .into(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("https://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let worker = thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(2);
            let stream = loop {
                if let Ok((stream, _)) = listener.accept() {
                    break stream;
                }
                if Instant::now() >= end {
                    return;
                }
                thread::sleep(Duration::from_millis(2));
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut tls = rustls::StreamOwned::new(
                rustls::ServerConnection::new(Arc::new(config)).unwrap(),
                stream,
            );
            let mut request = [0; 4096];
            let mut length = 0;
            while length < request.len() {
                match tls.read(&mut request[length..]) {
                    Ok(0) | Err(_) => return,
                    Ok(read) => length += read,
                }
                if request[..length]
                    .windows(4)
                    .any(|bytes| bytes == b"\r\n\r\n")
                {
                    break;
                }
            }
            for (delay, bytes) in parts {
                thread::sleep(delay);
                if tls.write_all(&bytes).and_then(|()| tls.flush()).is_err() {
                    return;
                }
            }
            tls.conn.send_close_notify();
            let _ = tls.flush();
        });
        (Http { client }, origin, Server(Some(worker)))
    }
    fn request(origin: String, target: &'static str) -> PreparedRequest {
        PreparedRequest {
            id: 1,
            origin,
            target,
            body: vec![1],
            proof: None,
        }
    }
    #[tokio::test]
    async fn actual_tls_accepts_bootstrap_bound_and_refuses_oversize_or_redirect() {
        let mut response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 32768\r\nConnection: close\r\n\r\n".to_vec();
        response.extend(std::iter::repeat_n(1, 32768));
        let (http, origin, _server) = server(vec![(Duration::ZERO, response)]);
        assert_eq!(
            http.send(&request(origin, "/peer/v3/bootstrap"))
                .await
                .unwrap()
                .len(),
            32768
        );
        let (http, origin, _server) = server(vec![(
            Duration::ZERO,
            b"HTTP/1.1 200 OK\r\nContent-Length: 8193\r\nConnection: close\r\n\r\n".to_vec(),
        )]);
        assert_eq!(
            http.send(&request(origin, "/peer/v3/challenge")).await,
            Err(Error::Capacity)
        );
        let (http, origin, _server) = server(vec![(Duration::ZERO, b"HTTP/1.1 302 Found\r\nLocation: https://example.test/secret\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx".to_vec())]);
        assert_eq!(
            http.send(&request(origin, "/peer/v3/challenge")).await,
            Err(Error::Status)
        );
    }
    #[tokio::test]
    async fn actual_tls_chunk_stream_obeys_one_absolute_deadline_and_total_bound() {
        let mut response =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2001\r\n"
                .to_vec();
        response.extend(std::iter::repeat_n(1, 8193));
        response.extend_from_slice(b"\r\n0\r\n\r\n");
        let (http, origin, _server) = server(vec![(Duration::ZERO, response)]);
        assert_eq!(
            http.send(&request(origin, "/peer/v3/challenge")).await,
            Err(Error::Capacity)
        );
        let mut parts = vec![(
            Duration::ZERO,
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec(),
        )];
        for _ in 0..5 {
            parts.push((Duration::from_millis(40), b"1\r\nx\r\n".to_vec()));
        }
        parts.push((Duration::ZERO, b"0\r\n\r\n".to_vec()));
        let (http, origin, _server) = server(parts);
        assert_eq!(
            http.send_for(
                &request(origin, "/peer/v3/challenge"),
                Duration::from_millis(100)
            )
            .await,
            Err(Error::Timeout)
        );
    }
}
