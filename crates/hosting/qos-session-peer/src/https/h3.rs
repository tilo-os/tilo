//! HTTP/3 carrier for exactly the routes the HTTPS listener serves.
//!
//! The host's single QUIC endpoint (same port as the TCP listener) accepts both
//! WebTransport sessions (media, live) and plain HTTP/3 connections. Browsers
//! open separate QUIC connections for the two, so the dispatcher classifies a
//! connection by its first request stream: an extended CONNECT for
//! WebTransport is handed to `web_transport_quinn` unchanged; any other request
//! makes the connection an HTTP/3 request connection.
//!
//! Every HTTP/3 request goes through the same `handle` as HTTP/1.1 and HTTP/2:
//! identical origin checks, route classification, body bounds, proofs, owner
//! dispatch and responses. This module only frames bytes (RFC 9114) with
//! static-table/literal QPACK and never advertises a dynamic table.
use super::*;
use web_transport_quinn::proto::{ConnectRequest, QpackHeaders, Settings};

const FRAME_DATA: u64 = 0x00;
const FRAME_HEADERS: u64 = 0x01;
const FRAME_GOAWAY: u64 = 0x07;
const H3_NO_ERROR: u32 = 0x100;
const H3_GENERAL_PROTOCOL_ERROR: u32 = 0x101;
const H3_FRAME_UNEXPECTED: u32 = 0x105;
const H3_EXCESSIVE_LOAD: u32 = 0x107;
const H3_REQUEST_REJECTED: u32 = 0x10b;
/// Unknown/reserved frames are skipped; their payloads are still bounded.
const MAX_SKIPPED_FRAME: u64 = 64 * 1024;
/// Concurrent request streams per HTTP/3 connection (HTTP/2 uses 16).
const MAX_STREAMS: usize = 16;

/// The HTTPS listener's route context, shared with HTTP/3 request streams.
#[derive(Clone)]
pub(crate) struct H3Service {
    pub(super) origin: Arc<Origin>,
    pub(super) assets: AssetRoot,
    pub(super) ingress: Ingress,
    pub(super) local_enrollment: Option<Arc<LocalEnrollment>>,
}

/// Result of classifying one accepted QUIC connection.
pub(crate) enum Accepted {
    /// Extended CONNECT: continue exactly as `web_transport_quinn` would.
    WebTransport(web_transport_quinn::Request),
    /// Plain HTTP/3 request connection.
    Http(HttpConnection),
}

/// An HTTP/3 connection whose first request header block was already read.
pub(crate) struct HttpConnection {
    conn: quinn::Connection,
    control: quinn::SendStream,
    peer_control: quinn::RecvStream,
    first: (quinn::SendStream, quinn::RecvStream, Vec<u8>),
}

#[derive(Debug)]
pub(crate) enum H3Error {
    Connection(quinn::ConnectionError),
    Closed,
    Read(String),
    Write(String),
    Protocol(&'static str),
}
impl From<quinn::ConnectionError> for H3Error {
    fn from(error: quinn::ConnectionError) -> Self {
        Self::Connection(error)
    }
}
impl std::fmt::Display for H3Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connection(error) => write!(f, "HTTP/3 connection: {error}"),
            Self::Closed => f.write_str("HTTP/3 stream closed early"),
            Self::Read(error) => write!(f, "HTTP/3 read: {error}"),
            Self::Write(error) => write!(f, "HTTP/3 write: {error}"),
            Self::Protocol(error) => write!(f, "HTTP/3 protocol: {error}"),
        }
    }
}
impl H3Error {
    fn code(&self) -> u32 {
        match self {
            Self::Protocol("frame unexpected") => H3_FRAME_UNEXPECTED,
            Self::Protocol("excessive load") => H3_EXCESSIVE_LOAD,
            Self::Protocol(_) => H3_GENERAL_PROTOCOL_ERROR,
            _ => H3_REQUEST_REJECTED,
        }
    }
}

/// HTTP/3 request serving and its `Alt-Svc` advertisement (default on).
pub(crate) fn enabled() -> bool {
    std::env::var("QOS_HTTPS_H3").map_or(true, |value| value != "0")
}

/// Exchange SETTINGS and read the first request header block.
pub(crate) async fn accept(conn: quinn::Connection) -> Result<Accepted, H3Error> {
    let open = async {
        let mut settings = Settings::default();
        // Same SETTINGS the WebTransport server sends; harmless for plain
        // requests (no QPACK dynamic table is offered).
        settings.enable_webtransport(1);
        let mut send = conn.open_uni().await?;
        settings
            .write(&mut send)
            .await
            .map_err(|e| H3Error::Write(e.to_string()))?;
        Ok::<_, H3Error>(send)
    };
    let peer = async {
        let mut recv = conn.accept_uni().await?;
        let settings = Settings::read(&mut recv)
            .await
            .map_err(|e| H3Error::Read(e.to_string()))?;
        Ok::<_, H3Error>((recv, settings))
    };
    let (control, (peer_control, peer_settings)) = tokio::try_join!(open, peer)?;
    let (send, mut recv) = conn.accept_bi().await?;
    let (frame, payload_at) = read_headers_frame(&mut recv).await?;
    let fields = QpackHeaders::decode_fields(&mut &frame[payload_at..])
        .map_err(|_| H3Error::Protocol("invalid field section"))?;
    let connect = fields
        .iter()
        .any(|(name, value)| name == ":method" && value == "CONNECT");
    if connect {
        if peer_settings.supports_webtransport() == 0 {
            return Err(H3Error::Protocol("WebTransport unsupported"));
        }
        let request = ConnectRequest::decode(&mut frame.as_slice())
            .map_err(|_| H3Error::Protocol("invalid CONNECT"))?;
        return Ok(Accepted::WebTransport(
            web_transport_quinn::Request::from_parts(
                conn,
                control,
                peer_control,
                request,
                send,
                recv,
            ),
        ));
    }
    Ok(Accepted::Http(HttpConnection {
        conn,
        control,
        peer_control,
        first: (send, recv, frame[payload_at..].to_vec()),
    }))
}

impl H3Service {
    /// Serve request streams until the connection closes or reaches the same
    /// absolute lifetime as HTTP/2 connections, then GOAWAY and drain.
    pub(crate) async fn serve(self, connection: HttpConnection) {
        let HttpConnection {
            conn,
            mut control,
            peer_control,
            first,
        } = connection;
        // The client's control and QPACK streams carry only small frames
        // (SETTINGS, PRIORITY_UPDATE, GOAWAY); drain them so their flow
        // control never stalls the peer.
        let mut background = JoinSet::new();
        background.spawn(drain(peer_control));
        let uni = conn.clone();
        background.spawn(async move {
            let mut streams = JoinSet::new();
            while let Ok(recv) = uni.accept_uni().await {
                if streams.len() >= 8 {
                    let mut recv = recv;
                    let _ = recv.stop(quinn::VarInt::from_u32(H3_EXCESSIVE_LOAD));
                    continue;
                }
                streams.spawn(drain(recv));
            }
        });
        let mut streams = JoinSet::new();
        let (send, recv, block) = first;
        let mut last_id = quinn::VarInt::from(send.id()).into_inner();
        streams.spawn(self.clone().stream(send, recv, Some(block)));
        let expires = Instant::now() + CONNECTION_LIFETIME;
        loop {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(expires) => break,
                _ = streams.join_next(), if !streams.is_empty() => {}
                accepted = conn.accept_bi() => {
                    let Ok((send, mut recv)) = accepted else { break };
                    last_id = quinn::VarInt::from(send.id()).into_inner();
                    if streams.len() >= MAX_STREAMS {
                        let mut send = send;
                        let _ = recv.stop(quinn::VarInt::from_u32(H3_REQUEST_REJECTED));
                        let _ = send.reset(quinn::VarInt::from_u32(H3_REQUEST_REJECTED));
                        continue;
                    }
                    streams.spawn(self.clone().stream(send, recv, None));
                }
            }
        }
        // Graceful end: GOAWAY (no request after `last_id` will be served),
        // then a bounded drain of the requests already in flight.
        let mut goaway = Vec::new();
        put_varint(&mut goaway, FRAME_GOAWAY);
        put_varint(&mut goaway, varint_len(last_id + 4) as u64);
        put_varint(&mut goaway, last_id + 4);
        let _ = control.write_all(&goaway).await;
        let _ = timeout(CONNECTION_GRACE, async {
            while streams.join_next().await.is_some() {}
        })
        .await;
        conn.close(quinn::VarInt::from_u32(H3_NO_ERROR), b"");
        background.abort_all();
    }

    async fn stream(
        self,
        mut send: quinn::SendStream,
        mut recv: quinn::RecvStream,
        block: Option<Vec<u8>>,
    ) {
        match self.request(&mut send, &mut recv, block).await {
            Ok(()) => {}
            Err(error) => {
                tracing::debug!(%error, "http3 request stream failed");
                let code = quinn::VarInt::from_u32(error.code());
                let _ = recv.stop(code);
                let _ = send.reset(code);
            }
        }
    }

    async fn request(
        &self,
        send: &mut quinn::SendStream,
        recv: &mut quinn::RecvStream,
        block: Option<Vec<u8>>,
    ) -> Result<(), H3Error> {
        let (head, body) = timeout(DEADLINE, async {
            let block = match block {
                Some(block) => block,
                None => {
                    let (frame, at) = read_headers_frame(recv).await?;
                    frame[at..].to_vec()
                }
            };
            let fields = QpackHeaders::decode_fields(&mut block.as_slice())
                .map_err(|_| H3Error::Protocol("invalid field section"))?;
            let head = request_head(fields)?;
            let body = read_body(recv).await?;
            Ok::<_, H3Error>((head, body))
        })
        .await
        .map_err(|_| H3Error::Protocol("request deadline"))??;
        let response = match head {
            Ok(builder) => {
                let request = builder
                    .body(http_body_util::Full::new(body))
                    .map_err(|_| H3Error::Protocol("invalid request head"))?;
                let Ok(response) = handle(
                    request,
                    self.origin.clone(),
                    self.assets.clone(),
                    self.ingress.clone(),
                    self.local_enrollment.clone(),
                )
                .await;
                response
            }
            Err(rejection) => reject(rejection),
        };
        write_response(send, response).await
    }
}

/// Build the request head from an HTTP/3 field section (RFC 9114 4.3.1).
/// Malformed requests become the same 400 rejection HTTP/2 would produce.
fn request_head(
    fields: Vec<(String, String)>,
) -> Result<Result<hyper::http::request::Builder, Rejection>, H3Error> {
    let (mut method, mut scheme, mut authority, mut path) = (None, None, None, None);
    let mut cookies = Vec::new();
    let mut builder = Request::builder().version(Version::HTTP_3);
    let mut regular = false;
    for (name, value) in fields {
        if let Some(pseudo) = name.strip_prefix(':') {
            if regular {
                return Ok(Err(Rejection::invalid()));
            }
            let slot = match pseudo {
                "method" => &mut method,
                "scheme" => &mut scheme,
                "authority" => &mut authority,
                "path" => &mut path,
                // `:protocol` is only valid with CONNECT, which this
                // connection does not serve.
                _ => return Ok(Err(Rejection::invalid())),
            };
            if slot.replace(value).is_some() {
                return Ok(Err(Rejection::invalid()));
            }
            continue;
        }
        regular = true;
        if name.bytes().any(|b| b.is_ascii_uppercase())
            || matches!(
                name.as_str(),
                "connection" | "keep-alive" | "proxy-connection" | "transfer-encoding" | "upgrade"
            )
            || (name == "te" && value != "trailers")
        {
            return Ok(Err(Rejection::invalid()));
        }
        // Split cookie crumbs are rejoined into one field (RFC 9114 4.2.1).
        if name == "cookie" {
            cookies.push(value);
            continue;
        }
        builder = builder.header(name.as_str(), value.as_str());
    }
    if !cookies.is_empty() {
        builder = builder.header("cookie", cookies.join("; "));
    }
    let (Some(method), Some(scheme), Some(authority), Some(path)) =
        (method, scheme, authority, path)
    else {
        return Ok(Err(Rejection::invalid()));
    };
    if method == "CONNECT" || scheme != "https" || path.is_empty() {
        return Ok(Err(Rejection::invalid()));
    }
    let Ok(uri) = format!("https://{authority}{path}").parse::<hyper::Uri>() else {
        return Ok(Err(Rejection::invalid()));
    };
    Ok(Ok(builder.method(method.as_str()).uri(uri)))
}

/// Read DATA frames to FIN within the largest route body bound; the route
/// handler then applies its exact per-route limit and reservation.
async fn read_body(recv: &mut quinn::RecvStream) -> Result<Bytes, H3Error> {
    let mut body = BytesMut::new();
    let mut trailers = false;
    while let Some((kind, len)) = read_frame_header(recv).await? {
        match kind {
            FRAME_DATA if !trailers => {
                let len = usize::try_from(len).map_err(|_| H3Error::Protocol("excessive load"))?;
                if len > BODY_BYTES - body.len() {
                    return Err(H3Error::Protocol("excessive load"));
                }
                let start = body.len();
                body.resize(start + len, 0);
                recv.read_exact(&mut body[start..])
                    .await
                    .map_err(|e| H3Error::Read(e.to_string()))?;
            }
            // Trailers are accepted and ignored (the listener rejects
            // `trailer`-announcing requests at the head); nothing may follow.
            FRAME_HEADERS if !trailers => {
                trailers = true;
                skip(recv, len).await?;
            }
            FRAME_DATA | FRAME_HEADERS => return Err(H3Error::Protocol("frame unexpected")),
            _ if reserved_request_frame(kind) => return Err(H3Error::Protocol("frame unexpected")),
            _ => skip(recv, len).await?,
        }
    }
    Ok(body.freeze())
}

async fn write_response(
    send: &mut quinn::SendStream,
    response: Response<ResponseBody>,
) -> Result<(), H3Error> {
    let (parts, mut body) = response.into_parts();
    let mut headers = QpackHeaders::default();
    headers.set(":status", parts.status.as_str());
    for (name, value) in &parts.headers {
        if matches!(
            name.as_str(),
            "connection" | "keep-alive" | "transfer-encoding" | "upgrade" | "alt-svc"
        ) {
            continue;
        }
        let Ok(value) = value.to_str() else { continue };
        headers.set(name.as_str(), value);
    }
    let mut block = BytesMut::new();
    headers.encode(&mut block);
    let mut out = Vec::with_capacity(block.len() + 16);
    put_varint(&mut out, FRAME_HEADERS);
    put_varint(&mut out, block.len() as u64);
    out.extend_from_slice(&block);
    send.write_all(&out)
        .await
        .map_err(|e| H3Error::Write(e.to_string()))?;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| H3Error::Write(e.to_string()))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if data.is_empty() {
            continue;
        }
        let mut header = Vec::with_capacity(16);
        put_varint(&mut header, FRAME_DATA);
        put_varint(&mut header, data.len() as u64);
        send.write_all(&header)
            .await
            .map_err(|e| H3Error::Write(e.to_string()))?;
        send.write_chunk(data)
            .await
            .map_err(|e| H3Error::Write(e.to_string()))?;
    }
    send.finish().map_err(|e| H3Error::Write(e.to_string()))?;
    Ok(())
}

/// Frame types reserved from HTTP/2 or defined for other streams (RFC 9114
/// 7.2.8 and 7.2.4-7.2.7): receiving them on a request stream is an error.
fn reserved_request_frame(kind: u64) -> bool {
    matches!(
        kind,
        0x02 | 0x03 | 0x04 | 0x05 | 0x06 | 0x07 | 0x08 | 0x09 | 0x0d
    )
}

/// Read one complete HEADERS frame (skipping unknown frames before it).
/// Returns the frame bytes and the payload offset.
async fn read_headers_frame(recv: &mut quinn::RecvStream) -> Result<(Vec<u8>, usize), H3Error> {
    loop {
        let (kind, len) = read_frame_header(recv).await?.ok_or(H3Error::Closed)?;
        match kind {
            FRAME_HEADERS => {
                let len = usize::try_from(len).map_err(|_| H3Error::Protocol("excessive load"))?;
                if len > HEADER_BYTES {
                    return Err(H3Error::Protocol("excessive load"));
                }
                let mut frame = Vec::with_capacity(len + 16);
                put_varint(&mut frame, FRAME_HEADERS);
                put_varint(&mut frame, len as u64);
                let at = frame.len();
                frame.resize(at + len, 0);
                recv.read_exact(&mut frame[at..])
                    .await
                    .map_err(|e| H3Error::Read(e.to_string()))?;
                return Ok((frame, at));
            }
            FRAME_DATA => return Err(H3Error::Protocol("frame unexpected")),
            kind if reserved_request_frame(kind) => {
                return Err(H3Error::Protocol("frame unexpected"));
            }
            _ => skip(recv, len).await?,
        }
    }
}

async fn skip(recv: &mut quinn::RecvStream, len: u64) -> Result<(), H3Error> {
    if len > MAX_SKIPPED_FRAME {
        return Err(H3Error::Protocol("excessive load"));
    }
    let mut scratch = vec![0; len as usize];
    recv.read_exact(&mut scratch)
        .await
        .map_err(|e| H3Error::Read(e.to_string()))
}

/// `None` at a clean FIN before any byte of a new frame.
async fn read_frame_header(recv: &mut quinn::RecvStream) -> Result<Option<(u64, u64)>, H3Error> {
    let Some(kind) = read_varint(recv, true).await? else {
        return Ok(None);
    };
    let len = read_varint(recv, false).await?.ok_or(H3Error::Closed)?;
    Ok(Some((kind, len)))
}

async fn read_varint(recv: &mut quinn::RecvStream, fin_ok: bool) -> Result<Option<u64>, H3Error> {
    let mut first = [0u8; 1];
    match recv
        .read(&mut first)
        .await
        .map_err(|e| H3Error::Read(e.to_string()))?
    {
        None if fin_ok => return Ok(None),
        None | Some(0) => return Err(H3Error::Closed),
        Some(_) => {}
    }
    let extra = (1usize << (first[0] >> 6)) - 1;
    let mut rest = [0u8; 7];
    recv.read_exact(&mut rest[..extra])
        .await
        .map_err(|e| H3Error::Read(e.to_string()))?;
    let mut value = u64::from(first[0] & 0x3f);
    for byte in &rest[..extra] {
        value = (value << 8) | u64::from(*byte);
    }
    Ok(Some(value))
}

fn varint_len(value: u64) -> usize {
    match value {
        0..=63 => 1,
        64..=16383 => 2,
        16384..=1_073_741_823 => 4,
        _ => 8,
    }
}

fn put_varint(out: &mut Vec<u8>, value: u64) {
    match varint_len(value) {
        1 => out.push(value as u8),
        2 => out.extend_from_slice(&((value as u16) | 0x4000).to_be_bytes()),
        4 => out.extend_from_slice(&((value as u32) | 0x8000_0000).to_be_bytes()),
        _ => out.extend_from_slice(&(value | 0xc000_0000_0000_0000).to_be_bytes()),
    }
}

/// Discard a peer unidirectional stream (control, QPACK, unknown types).
async fn drain(mut recv: quinn::RecvStream) {
    let mut scratch = [0u8; 4096];
    loop {
        match recv.read(&mut scratch).await {
            Ok(Some(_)) => {}
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests;
