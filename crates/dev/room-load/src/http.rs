//! Plain HTTP/1.1 to one loopback origin with kept-alive connections, as a
//! browser's `fetch` reuses them: the relay pays for requests, not for a TCP
//! accept per long-poll.

use std::net::SocketAddr;
use std::sync::Mutex;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use tilo_browser_peer::{HttpClient, HttpFuture};
use tilo_lock::MutexRecover as _;

type Sender = SendRequest<Full<Bytes>>;

/// Connections to one origin.
pub struct Plain {
    addr: SocketAddr,
    idle: Mutex<Vec<Sender>>,
    /// Bytes of every answer read whole: what the host sent this origin.
    pub received: std::sync::atomic::AtomicU64,
}

impl Plain {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            idle: Mutex::default(),
            received: std::sync::atomic::AtomicU64::new(0),
        }
    }

    async fn sender(&self) -> Result<Sender, String> {
        loop {
            let reused = self.idle.lock_recover().pop();
            match reused {
                Some(mut sender) => {
                    if sender.ready().await.is_ok() {
                        return Ok(sender);
                    }
                }
                None => break,
            }
        }
        let tcp = tokio::net::TcpStream::connect(self.addr)
            .await
            .map_err(|error| error.to_string())?;
        let _ = tcp.set_nodelay(true);
        let (sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tcp))
                .await
                .map_err(|error| error.to_string())?;
        tokio::spawn(connection);
        Ok(sender)
    }

    fn keep(&self, sender: Sender) {
        if !sender.is_closed() {
            self.idle.lock_recover().push(sender);
        }
    }

    /// `POST path` and the answer's status and streamed body. The
    /// connection is not reused: a streamed body holds it.
    pub async fn open(
        &self,
        path: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Result<(u16, Incoming), String> {
        let mut sender = self.sender().await?;
        let mut request = hyper::Request::post(path).header(hyper::header::HOST, "localhost");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = request
            .body(Full::new(Bytes::from(body)))
            .map_err(|error| error.to_string())?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|error| error.to_string())?;
        Ok((response.status().as_u16(), response.into_body()))
    }

    /// `POST path` and the whole answer.
    pub async fn post_with(
        &self,
        path: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Result<(u16, Vec<u8>), String> {
        let mut sender = self.sender().await?;
        let mut request = hyper::Request::post(path).header(hyper::header::HOST, "localhost");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = request
            .body(Full::new(Bytes::from(body)))
            .map_err(|error| error.to_string())?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|error| error.to_string())?
            .to_bytes()
            .to_vec();
        self.keep(sender);
        self.received
            .fetch_add(body.len() as u64, std::sync::atomic::Ordering::Relaxed);
        Ok((status, body))
    }
}

impl HttpClient for Plain {
    fn post(&self, path: &str, session: Option<&str>, body: Vec<u8>) -> HttpFuture<'_> {
        let path = path.to_owned();
        let session = session.map(str::to_owned);
        Box::pin(async move {
            let mut headers = vec![("content-type", "application/octet-stream")];
            if let Some(session) = &session {
                headers.push((tilo_relay_protocol::web::SESSION_HEADER, session.as_str()));
            }
            self.post_with(&path, &headers, body).await
        })
    }
}
