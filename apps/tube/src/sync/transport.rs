//! Live Grayjay "Sync" transport over the Noise Protocol Framework.
//!
//! This transport is gated behind the `grayjay-sync` cargo feature:
//! a blocking TCP client that runs Grayjay's exact handshake and framing, so it
//! can pair with and sync subscriptions/history to/from a phone or desktop
//! running Grayjay. Every wire detail is a clean-room reimplementation of the
//! facts documented and cited in [`super::protocol`].
//!
//! ## Sequence (initiator = us, per `SyncSocketSession.kt` + `SyncService.kt`)
//!
//! 1. **Version check** on the raw socket: write `PROTOCOL_VERSION` (u32 LE),
//!    read the peer's, require `>= MINIMUM_PROTOCOL_VERSION`.
//! 2. **Noise handshake** (`Noise_IK_25519_ChaChaPoly_BLAKE2b`). The first frame
//!    is `[messageSize u32][appId u32][pairingLen u32][pairingMsg][mainMsg]`;
//!    the response is `[len u32][mainMsg2]`. When pairing, `pairingMsg` is a
//!    `Noise_N` handshake message carrying the UTF-8 pairing code.
//! 3. **Authorize**: exchange `NOTIFY/AUTHORIZED` frames carrying
//!    `[idLen u8][uuid][nameLen u8][deviceName]`. The session is usable once both
//!    directions have authorized.
//! 4. **DATA** frames: `[len u32][ Noise( [size u32][opcode][subOpcode][ce]
//!    [payload] ) ]`, payload = gzip'd JSON of a sync package. Oversized payloads
//!    are chunked with STREAM (START/DATA/END).

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::io::{self, Read, Write};
use std::pin::pin;
use std::task::Poll;
use ui::time::{Duration, Instant};

use base64::Engine;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use snow::params::DHChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use snow::{Builder, HandshakeState, TransportState};
use tilo_clock::unix_now_i64;
use zeroize::Zeroizing;

use super::protocol::{
    self, Frame, HEADER_SIZE, MAXIMUM_PACKET_SIZE, MAXIMUM_PACKET_SIZE_ENCRYPTED,
    MINIMUM_PROTOCOL_VERSION, NOISE_PATTERN_MAIN, NOISE_PATTERN_PAIRING, PROTOCOL_VERSION,
    SerializedChannel, Subscription, SyncDeviceInfo, SyncSessionData, SyncSubscriptionsPackage,
    content_encoding, notify, opcode, stream, sync_opcode,
};
use super::{PeerDescriptor, PeerId, SyncError, SyncResult, SyncState, SyncTransport};

/// A subscription/history exchange is metadata, not a media transfer.  This
/// cap bounds streamed reassembly, gzip expansion, and JSON allocation even if
/// an authenticated peer is buggy or hostile.
const MAX_SYNC_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONCURRENT_STREAMS: usize = 4;

fn b64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn b64_decode(s: &str) -> SyncResult<Vec<u8>> {
    let s = s.trim();
    for eng in [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ] {
        if let Ok(v) = eng.decode(s) {
            return Ok(v);
        }
    }
    Err(SyncError::Pairing("invalid base64 value".into()))
}

const MAX_PAIRING_BLOB_BYTES: usize = 16 * 1024;
const MAX_PEER_ADDRESSES: usize = 8;
const MAX_PEER_ADDRESS_BYTES: usize = 255;
const MAX_PAIRING_CODE_BYTES: usize = 1024;

fn validate_device_info(info: &SyncDeviceInfo) -> SyncResult<()> {
    let key = b64_decode(&info.public_key)?;
    if key.len() != 32
        || info.addresses.is_empty()
        || info.addresses.len() > MAX_PEER_ADDRESSES
        || info
            .addresses
            .iter()
            .any(|address| address.is_empty() || address.len() > MAX_PEER_ADDRESS_BYTES)
        || !(1..=i32::from(u16::MAX)).contains(&info.port)
        || info
            .pairing_code
            .as_ref()
            .is_some_and(|code| code.len() > MAX_PAIRING_CODE_BYTES)
    {
        return Err(SyncError::Pairing(
            "pairing metadata is malformed or exceeds its bounds".into(),
        ));
    }
    Ok(())
}

fn snow_err(e: &snow::Error) -> SyncError {
    SyncError::Transport(format!("noise: {e}"))
}

fn io_err(e: &std::io::Error) -> SyncError {
    SyncError::Transport(format!("io: {e}"))
}

#[derive(Clone)]
pub struct DeviceKey {
    private: Zeroizing<Vec<u8>>,
    pub public: Vec<u8>,
}

impl DeviceKey {
    pub fn generate() -> SyncResult<Self> {
        let params = NOISE_PATTERN_MAIN
            .parse()
            .map_err(|error| snow_err(&error))?;
        let kp = Builder::new(params)
            .generate_keypair()
            .map_err(|error| snow_err(&error))?;
        Ok(DeviceKey {
            private: Zeroizing::new(kp.private),
            public: kp.public,
        })
    }

    pub fn from_private(private: &[u8]) -> SyncResult<Self> {
        if private.len() != 32 {
            return Err(SyncError::KeyUnavailable(
                "device key material has an invalid length".into(),
            ));
        }
        let mut dh = DefaultResolver
            .resolve_dh(&DHChoice::Curve25519)
            .ok_or_else(|| SyncError::KeyUnavailable("Curve25519 is unavailable".into()))?;
        dh.set(private);
        Ok(Self {
            private: Zeroizing::new(private.to_vec()),
            public: dh.pubkey().to_vec(),
        })
    }

    pub fn public_base64(&self) -> String {
        b64_encode(&self.public)
    }
}

#[derive(Clone, Debug)]
enum Incoming {
    Data { sub_opcode: u8, payload: Vec<u8> },
    Authorized { remote_name: String },
    Unauthorized,
}

struct Reasm {
    opcode: u8,
    sub_opcode: u8,
    content_encoding: u8,
    expected_size: usize,
    buf: Vec<u8>,
}

pub struct GrayjaySession {
    stream: Stream,
    transport: TransportState,
    remote_public_key: String,
    remote_version: i32,
    remote_authorized: bool,
    streams: HashMap<i32, Reasm>,
    next_stream_id: i32,
    device_name: String,
}

impl GrayjaySession {
    pub fn remote_public_key(&self) -> &str {
        &self.remote_public_key
    }

    pub fn remote_version(&self) -> i32 {
        self.remote_version
    }

    fn version_check(stream: &mut Stream) -> SyncResult<i32> {
        stream
            .write_all(&PROTOCOL_VERSION.to_le_bytes())
            .map_err(|error| io_err(&error))?;
        let mut buf = [0u8; 4];
        read_exact(stream, &mut buf)?;
        let remote = i32::from_le_bytes(buf);
        if remote < MINIMUM_PROTOCOL_VERSION {
            return Err(SyncError::Transport(format!(
                "peer protocol version {remote} < minimum {MINIMUM_PROTOCOL_VERSION}"
            )));
        }
        Ok(remote)
    }

    pub fn connect_initiator(
        stream: impl Into<Stream>,
        device: &DeviceKey,
        remote_public_key_b64: &str,
        pairing_code: Option<&str>,
        device_name: &str,
    ) -> SyncResult<Self> {
        let remote_pub = b64_decode(remote_public_key_b64)?;

        let mut stream: Stream = stream.into();
        let remote_version = Self::version_check(&mut stream)?;

        let params = NOISE_PATTERN_MAIN
            .parse()
            .map_err(|error| snow_err(&error))?;
        let mut hs: HandshakeState = Builder::new(params)
            .local_private_key(&device.private)
            .remote_public_key(&remote_pub)
            .build_initiator()
            .map_err(|error| snow_err(&error))?;

        let pairing_msg: Vec<u8> = if let Some(code) = pairing_code {
            let pairing_params = NOISE_PATTERN_PAIRING
                .parse()
                .map_err(|error| snow_err(&error))?;
            let mut ph: HandshakeState = Builder::new(pairing_params)
                .remote_public_key(&remote_pub)
                .build_initiator()
                .map_err(|error| snow_err(&error))?;
            let mut buf = vec![0u8; 512];
            let n = ph
                .write_message(code.as_bytes(), &mut buf)
                .map_err(|error| snow_err(&error))?;
            buf.truncate(n);
            buf
        } else {
            Vec::new()
        };

        let mut main_buf = vec![0u8; 512];
        let main_len = hs
            .write_message(&[], &mut main_buf)
            .map_err(|error| snow_err(&error))?;

        let app_id: u32 = 0;
        let mut message = Vec::with_capacity(4 + 4 + pairing_msg.len() + main_len);
        message.extend_from_slice(&app_id.to_le_bytes());
        message.extend_from_slice(&(pairing_msg.len() as u32).to_le_bytes());
        message.extend_from_slice(&pairing_msg);
        let main_message = main_buf.get(..main_len).ok_or_else(|| {
            SyncError::Transport("Noise produced an invalid handshake length".into())
        })?;
        message.extend_from_slice(main_message);
        write_frame(&mut stream, &message)?;

        let resp = read_frame(&mut stream)?;
        let mut scratch = vec![0u8; 512];
        hs.read_message(&resp, &mut scratch)
            .map_err(|error| snow_err(&error))?;

        // Capture + verify the peer's static key before consuming the handshake.
        let remote_static = hs
            .get_remote_static()
            .map(<[u8]>::to_vec)
            .ok_or_else(|| SyncError::Transport("peer did not present a static key".into()))?;
        let remote_static_b64 = b64_encode(&remote_static);
        if remote_static_b64 != b64_encode(&remote_pub) {
            return Err(SyncError::Pairing(format!(
                "peer key mismatch: expected {remote_public_key_b64}, got {remote_static_b64}"
            )));
        }

        let transport = hs.into_transport_mode().map_err(|error| snow_err(&error))?;

        Ok(GrayjaySession {
            stream,
            transport,
            remote_public_key: remote_static_b64,
            remote_version,
            remote_authorized: false,
            streams: HashMap::new(),
            next_stream_id: 0,
            device_name: device_name.to_string(),
        })
    }

    #[cfg(test)]
    fn accept_responder(
        stream: impl Into<Stream>,
        device: &DeviceKey,
        device_name: &str,
        allow: impl Fn(&str, Option<&str>) -> bool,
    ) -> SyncResult<Self> {
        let mut stream: Stream = stream.into();
        let remote_version = Self::version_check(&mut stream)?;

        let params = NOISE_PATTERN_MAIN
            .parse()
            .map_err(|error| snow_err(&error))?;
        let mut hs: HandshakeState = Builder::new(params)
            .local_private_key(&device.private)
            .build_responder()
            .map_err(|error| snow_err(&error))?;

        let msg = read_frame(&mut stream)?;
        if msg.len() < 8 {
            return Err(SyncError::Transport("handshake frame too short".into()));
        }
        let header: [u8; 8] = msg
            .get(..8)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| SyncError::Transport("handshake frame too short".into()))?;
        let app_id_bytes: [u8; 4] = header[..4]
            .try_into()
            .map_err(|_| SyncError::Transport("handshake frame too short".into()))?;
        let pairing_len_bytes: [u8; 4] = header[4..]
            .try_into()
            .map_err(|_| SyncError::Transport("handshake frame too short".into()))?;
        let _app_id = u32::from_le_bytes(app_id_bytes);
        let pairing_len = u32::from_le_bytes(pairing_len_bytes) as usize;
        let rest = msg
            .get(8..)
            .ok_or_else(|| SyncError::Transport("handshake frame too short".into()))?;
        if pairing_len > rest.len() {
            return Err(SyncError::Transport(
                "pairing length overflows frame".into(),
            ));
        }
        let (pairing_msg, main_msg) = rest.split_at(pairing_len);

        let pairing_code: Option<String> = if pairing_len > 0 {
            let pairing_params = NOISE_PATTERN_PAIRING
                .parse()
                .map_err(|error| snow_err(&error))?;
            let mut ph: HandshakeState = Builder::new(pairing_params)
                .local_private_key(&device.private)
                .build_responder()
                .map_err(|error| snow_err(&error))?;
            let mut pairing_plaintext = vec![0u8; 512];
            let n = ph
                .read_message(pairing_msg, &mut pairing_plaintext)
                .map_err(|error| snow_err(&error))?;
            let decoded = pairing_plaintext.get(..n).ok_or_else(|| {
                SyncError::Transport("Noise produced an invalid pairing length".into())
            })?;
            Some(String::from_utf8_lossy(decoded).into_owned())
        } else {
            None
        };

        let mut scratch = vec![0u8; 512];
        hs.read_message(main_msg, &mut scratch)
            .map_err(|error| snow_err(&error))?;

        let remote_static = hs
            .get_remote_static()
            .map(<[u8]>::to_vec)
            .ok_or_else(|| SyncError::Transport("initiator did not present a static key".into()))?;
        let remote_static_b64 = b64_encode(&remote_static);

        if remote_static_b64 == device.public_base64()
            || !allow(&remote_static_b64, pairing_code.as_deref())
        {
            return Err(SyncError::Pairing("handshake not allowed".into()));
        }

        let mut response_buffer = vec![0u8; 512];
        let rn = hs
            .write_message(&[], &mut response_buffer)
            .map_err(|error| snow_err(&error))?;
        let response = response_buffer.get(..rn).ok_or_else(|| {
            SyncError::Transport("Noise produced an invalid handshake length".into())
        })?;
        write_frame(&mut stream, response)?;

        let transport = hs.into_transport_mode().map_err(|error| snow_err(&error))?;

        Ok(GrayjaySession {
            stream,
            transport,
            remote_public_key: remote_static_b64,
            remote_version,
            remote_authorized: false,
            streams: HashMap::new(),
            next_stream_id: 0,
            device_name: device_name.to_string(),
        })
    }

    fn write_encrypted(&mut self, plaintext: &[u8]) -> SyncResult<()> {
        let mut cipher = vec![0u8; plaintext.len() + 16];
        let len = self
            .transport
            .write_message(plaintext, &mut cipher)
            .map_err(|error| snow_err(&error))?;
        let encrypted = cipher.get(..len).ok_or_else(|| {
            SyncError::Transport("Noise produced an invalid ciphertext length".into())
        })?;
        write_frame(&mut self.stream, encrypted)
    }

    fn read_encrypted(&mut self) -> SyncResult<Vec<u8>> {
        let cipher = read_frame(&mut self.stream)?;
        let mut plain = vec![0u8; cipher.len()];
        let plen = self
            .transport
            .read_message(&cipher, &mut plain)
            .map_err(|error| snow_err(&error))?;
        plain.truncate(plen);
        Ok(plain)
    }

    fn send(&mut self, opcode: u8, sub_opcode: u8, payload: &[u8], gzip: bool) -> SyncResult<()> {
        if payload.len() > MAX_SYNC_PAYLOAD_BYTES {
            return Err(SyncError::Transport(
                "outgoing sync payload exceeds resource bound".into(),
            ));
        }
        let (ce, processed) = if gzip && opcode == opcode::DATA {
            (content_encoding::GZIP, gzip_compress(payload)?)
        } else {
            (content_encoding::RAW, payload.to_vec())
        };
        if processed.len() > MAX_SYNC_PAYLOAD_BYTES {
            return Err(SyncError::Transport(
                "encoded sync payload exceeds resource bound".into(),
            ));
        }

        if processed.len() + HEADER_SIZE <= MAXIMUM_PACKET_SIZE {
            let plaintext = protocol::encode_frame(opcode, sub_opcode, ce, &processed);
            return self.write_encrypted(&plaintext);
        }

        let id = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        let segment_size = MAXIMUM_PACKET_SIZE - HEADER_SIZE;
        let total = processed.len();
        let mut offset = 0usize;
        while offset < total {
            let remaining = total - offset;
            let mut seg = Vec::with_capacity(segment_size);
            let (stream_op, bytes_to_send) = if offset == 0 {
                let bts = segment_size - 4 - HEADER_SIZE;
                seg.extend_from_slice(&id.to_le_bytes());
                seg.extend_from_slice(&(total as u32).to_le_bytes());
                seg.push(opcode);
                seg.push(sub_opcode);
                seg.push(ce);
                (stream::START, bts)
            } else {
                let bts = (segment_size - 4 - 4).min(remaining);
                let op = if bts >= remaining {
                    stream::END
                } else {
                    stream::DATA
                };
                seg.extend_from_slice(&id.to_le_bytes());
                seg.extend_from_slice(&(offset as u32).to_le_bytes());
                (op, bts)
            };
            let segment = processed
                .get(offset..offset + bytes_to_send)
                .ok_or_else(|| SyncError::Transport("stream segment bounds are invalid".into()))?;
            seg.extend_from_slice(segment);
            let plaintext =
                protocol::encode_frame(opcode::STREAM, stream_op, content_encoding::RAW, &seg);
            self.write_encrypted(&plaintext)?;
            offset += bytes_to_send;
        }
        Ok(())
    }

    pub fn send_data(&mut self, sub_opcode: u8, json: &str) -> SyncResult<()> {
        self.send(opcode::DATA, sub_opcode, json.as_bytes(), true)
    }

    pub fn send_authorized(&mut self) -> SyncResult<()> {
        let id = uuid::Uuid::new_v4().to_string();
        let name = self.device_name.clone();
        let id_bytes = id.as_bytes();
        let name_bytes = name.as_bytes();
        let mut payload = Vec::with_capacity(2 + id_bytes.len() + name_bytes.len());
        payload.push(id_bytes.len() as u8);
        payload.extend_from_slice(id_bytes);
        payload.push(name_bytes.len() as u8);
        payload.extend_from_slice(name_bytes);
        self.send(opcode::NOTIFY, notify::AUTHORIZED, &payload, false)
    }

    fn send_pong(&mut self) -> SyncResult<()> {
        self.send(opcode::PONG, 0, &[], false)
    }

    fn recv(&mut self) -> SyncResult<Incoming> {
        loop {
            let plain = self.read_encrypted()?;
            let frame = protocol::decode_frame(&plain)
                .map_err(|e| SyncError::Transport(format!("frame decode: {e}")))?;

            match frame.opcode {
                opcode::PING => {
                    self.send_pong()?;
                }
                opcode::STREAM => {
                    if let Some(resolved) = self.handle_stream(&frame)?
                        && let Some(inc) = Self::resolve(resolved)?
                    {
                        return Ok(inc);
                    }
                }
                opcode::NOTIFY => match frame.sub_opcode {
                    notify::AUTHORIZED => {
                        let name = parse_authorized_name(&frame.payload);
                        self.remote_authorized = true;
                        return Ok(Incoming::Authorized { remote_name: name });
                    }
                    notify::UNAUTHORIZED => {
                        self.remote_authorized = false;
                        return Ok(Incoming::Unauthorized);
                    }
                    _ => {}
                },
                opcode::DATA => {
                    if let Some(inc) = Self::resolve(frame)? {
                        return Ok(inc);
                    }
                }
                _ => {}
            }
        }
    }

    fn resolve(frame: Frame) -> SyncResult<Option<Incoming>> {
        if frame.opcode != opcode::DATA {
            return Ok(None);
        }
        let payload = if frame.content_encoding == content_encoding::GZIP {
            gzip_decompress(&frame.payload)?
        } else {
            frame.payload
        };
        Ok(Some(Incoming::Data {
            sub_opcode: frame.sub_opcode,
            payload,
        }))
    }

    fn handle_stream(&mut self, frame: &Frame) -> SyncResult<Option<Frame>> {
        let payload = frame.payload.as_slice();
        let short = || SyncError::Transport("stream segment too short".into());
        match frame.sub_opcode {
            stream::START => {
                let (id, expected, rest) = stream_header(payload)?;
                let [
                    stream_opcode,
                    stream_sub_opcode,
                    content_encoding,
                    initial @ ..,
                ] = rest
                else {
                    return Err(short());
                };
                if expected > MAX_SYNC_PAYLOAD_BYTES
                    || initial.len() > expected
                    || self.streams.len() >= MAX_CONCURRENT_STREAMS
                    || self.streams.contains_key(&id)
                {
                    return Err(SyncError::Transport(
                        "stream declaration exceeds resource bounds".into(),
                    ));
                }
                let mut buf = Vec::with_capacity(expected);
                buf.extend_from_slice(initial);
                self.streams.insert(
                    id,
                    Reasm {
                        opcode: *stream_opcode,
                        sub_opcode: *stream_sub_opcode,
                        content_encoding: *content_encoding,
                        expected_size: expected,
                        buf,
                    },
                );
                Ok(None)
            }
            stream::DATA => {
                let (id, expected_off, chunk) = stream_header(payload)?;
                let r = self
                    .streams
                    .get_mut(&id)
                    .ok_or_else(|| SyncError::Transport("stream data for unknown id".into()))?;
                if expected_off != r.buf.len() {
                    return Err(SyncError::Transport("stream offset mismatch".into()));
                }
                if chunk.len() > r.expected_size.saturating_sub(r.buf.len()) {
                    return Err(SyncError::Transport(
                        "stream data exceeds declared size".into(),
                    ));
                }
                r.buf.extend_from_slice(chunk);
                Ok(None)
            }
            stream::END => {
                let (id, expected_off, chunk) = stream_header(payload)?;
                let mut r = self
                    .streams
                    .remove(&id)
                    .ok_or_else(|| SyncError::Transport("stream end for unknown id".into()))?;
                if expected_off != r.buf.len() {
                    return Err(SyncError::Transport("stream offset mismatch at end".into()));
                }
                if chunk.len() > r.expected_size.saturating_sub(r.buf.len()) {
                    return Err(SyncError::Transport(
                        "stream end exceeds declared size".into(),
                    ));
                }
                r.buf.extend_from_slice(chunk);
                if r.buf.len() != r.expected_size {
                    return Err(SyncError::Transport(
                        "reassembled stream size mismatch".into(),
                    ));
                }
                Ok(Some(Frame {
                    opcode: r.opcode,
                    sub_opcode: r.sub_opcode,
                    content_encoding: r.content_encoding,
                    payload: r.buf,
                }))
            }
            _ => Ok(None),
        }
    }

    fn complete_authorization(&mut self, deadline: Instant) -> SyncResult<String> {
        self.send_authorized()?;
        loop {
            if ui::time::now() >= deadline {
                return Err(SyncError::Transport(
                    "timed out waiting for peer authorization".into(),
                ));
            }
            match self.recv()? {
                Incoming::Authorized { remote_name } => return Ok(remote_name),
                Incoming::Unauthorized => {
                    return Err(SyncError::Pairing("peer refused authorization".into()));
                }
                Incoming::Data { .. } => {}
            }
        }
    }
}

fn stream_header(payload: &[u8]) -> SyncResult<(i32, usize, &[u8])> {
    let header: [u8; 8] = payload
        .get(..8)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| SyncError::Transport("stream segment too short".into()))?;
    let id_bytes: [u8; 4] = header[..4]
        .try_into()
        .map_err(|_| SyncError::Transport("stream segment too short".into()))?;
    let offset_bytes: [u8; 4] = header[4..]
        .try_into()
        .map_err(|_| SyncError::Transport("stream segment too short".into()))?;
    let body = payload
        .get(8..)
        .ok_or_else(|| SyncError::Transport("stream segment too short".into()))?;
    Ok((
        i32::from_le_bytes(id_bytes),
        u32::from_le_bytes(offset_bytes) as usize,
        body,
    ))
}

pub struct Stream(Socket);

enum Socket {
    Net {
        socket: tilo_net::sockets::TcpStream,
        timeout: Duration,
    },
    #[cfg(test)]
    Std(std::net::TcpStream),
}

#[cfg(test)]
impl From<std::net::TcpStream> for Stream {
    fn from(stream: std::net::TcpStream) -> Self {
        Self(Socket::Std(stream))
    }
}

impl Stream {
    fn connect(
        host: &str,
        port: u16,
        connect_timeout: Duration,
        io_timeout: Duration,
    ) -> io::Result<Self> {
        let socket = wait(
            connect_timeout,
            tilo_net::sockets::TcpStream::connect_timeout(host, port, connect_timeout),
        )?;
        Ok(Self(Socket::Net {
            socket,
            timeout: io_timeout,
        }))
    }
}

fn wait<T>(
    timeout: Duration,
    future: impl Future<Output = Result<T, tilo_net::NetError>>,
) -> io::Result<T> {
    let bounded = async move {
        let mut future = pin!(future);
        let mut expiry = pin!(ui::rt::sleep(timeout));
        std::future::poll_fn(|cx| {
            if let Poll::Ready(result) = future.as_mut().poll(cx) {
                return Poll::Ready(Some(result));
            }
            expiry.as_mut().poll(cx).map(|()| None)
        })
        .await
    };
    match ui::rt::app().block_on(bounded) {
        Ok(Some(result)) => result.map_err(io::Error::other),
        Ok(None) => Err(io::ErrorKind::TimedOut.into()),
        Err(error) => Err(io::Error::other(error)),
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.0 {
            Socket::Net { socket, timeout } => {
                let timeout = *timeout;
                wait(timeout, socket.read(buf))
            }
            #[cfg(test)]
            Socket::Std(stream) => stream.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.0 {
            Socket::Net { socket, timeout } => {
                let timeout = *timeout;
                wait(timeout, socket.write_all(buf)).map(|()| buf.len())
            }
            #[cfg(test)]
            Socket::Std(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.0 {
            Socket::Net { .. } => Ok(()),
            #[cfg(test)]
            Socket::Std(stream) => stream.flush(),
        }
    }
}

fn read_exact(stream: &mut Stream, buf: &mut [u8]) -> SyncResult<()> {
    stream.read_exact(buf).map_err(|error| io_err(&error))
}

/// Every Grayjay sync packet: a little-endian `u32` length, then the Noise
/// handshake message or ciphertext.
const FRAMING: wire_frame::Framing =
    wire_frame::Framing::little_endian(MAXIMUM_PACKET_SIZE_ENCRYPTED);

fn write_frame(stream: &mut impl Write, payload: &[u8]) -> SyncResult<()> {
    let frame = FRAMING
        .encode(payload)
        .map_err(|error| SyncError::Transport(error.to_string()))?;
    stream.write_all(&frame).map_err(|error| io_err(&error))
}

fn read_frame(stream: &mut impl Read) -> SyncResult<Vec<u8>> {
    match FRAMING.read(stream) {
        Ok(Some(frame)) => Ok(frame),
        Ok(None) => Err(io_err(&std::io::ErrorKind::UnexpectedEof.into())),
        Err(wire_frame::FrameError::TooLarge(len)) => {
            Err(SyncError::Transport(format!("frame too large: {len}")))
        }
        Err(error) => Err(io_err(&error.into())),
    }
}

fn parse_authorized_name(payload: &[u8]) -> String {
    let Some((&id_len, rest)) = payload.split_first() else {
        return String::new();
    };
    let id_len = usize::from(id_len);
    let Some((&name_len, name)) = rest.get(id_len..).and_then(|tail| tail.split_first()) else {
        return String::new();
    };
    let end = usize::from(name_len).min(name.len());
    String::from_utf8_lossy(name.get(..end).unwrap_or_default()).into_owned()
}

fn gzip_compress(data: &[u8]) -> SyncResult<Vec<u8>> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).map_err(|error| io_err(&error))?;
    enc.finish().map_err(|error| io_err(&error))
}

fn gzip_decompress(data: &[u8]) -> SyncResult<Vec<u8>> {
    let mut dec = GzDecoder::new(data);
    let mut out = Vec::new();
    dec.by_ref()
        .take((MAX_SYNC_PAYLOAD_BYTES + 1) as u64)
        .read_to_end(&mut out)
        .map_err(|error| io_err(&error))?;
    if out.len() > MAX_SYNC_PAYLOAD_BYTES {
        return Err(SyncError::Transport(
            "decompressed sync payload exceeds resource bound".into(),
        ));
    }
    Ok(out)
}

pub struct GrayjaySyncTransport {
    device: DeviceKey,
    device_name: String,
    peers: HashMap<String, SyncDeviceInfo>,
    connect_timeout: Duration,
    io_timeout: Duration,
    sync_deadline: Duration,
}

impl GrayjaySyncTransport {
    pub fn with_device(device: DeviceKey, device_name: impl Into<String>) -> Self {
        GrayjaySyncTransport {
            device,
            device_name: device_name.into(),
            peers: HashMap::new(),
            connect_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(15),
            sync_deadline: Duration::from_secs(20),
        }
    }

    pub fn remember_peer(&mut self, info: SyncDeviceInfo) {
        self.peers.insert(info.public_key.clone(), info);
    }

    pub(crate) fn parse_blob(blob: &str) -> SyncResult<SyncDeviceInfo> {
        let blob = blob.trim();
        if blob.is_empty() || blob.len() > MAX_PAIRING_BLOB_BYTES {
            return Err(SyncError::Pairing(
                "pairing invite is empty or too large".into(),
            ));
        }
        if let Ok(info) = serde_json::from_str::<SyncDeviceInfo>(blob) {
            validate_device_info(&info)?;
            return Ok(info);
        }
        let payload = blob.rsplit(['/', ':']).next().unwrap_or(blob);
        let bytes = b64_decode(payload)?;
        if bytes.len() > MAX_PAIRING_BLOB_BYTES {
            return Err(SyncError::Pairing(
                "decoded pairing invite is too large".into(),
            ));
        }
        let info = serde_json::from_slice::<SyncDeviceInfo>(&bytes)
            .map_err(|e| SyncError::Pairing(format!("unrecognized pairing blob: {e}")))?;
        validate_device_info(&info)?;
        Ok(info)
    }

    fn open_session(
        &self,
        info: &SyncDeviceInfo,
        pairing_code: Option<&str>,
    ) -> SyncResult<GrayjaySession> {
        let mut last_err = SyncError::Transport("no addresses to connect".into());
        for addr in &info.addresses {
            let Ok(port) = u16::try_from(info.port) else {
                last_err = SyncError::Transport(format!("invalid port {}", info.port));
                continue;
            };
            let stream = match Stream::connect(addr, port, self.connect_timeout, self.io_timeout) {
                Ok(s) => s,
                Err(e) => {
                    last_err = io_err(&e);
                    continue;
                }
            };

            let mut session = GrayjaySession::connect_initiator(
                stream,
                &self.device,
                &info.public_key,
                pairing_code.or(info.pairing_code.as_deref()),
                &self.device_name,
            )?;
            let deadline = ui::time::now() + self.sync_deadline;
            session.complete_authorization(deadline)?;
            return Ok(session);
        }
        Err(last_err)
    }

    fn subscriptions_json(state: &SyncState) -> SyncResult<String> {
        let now = unix_now_i64();
        let pkg = SyncSubscriptionsPackage {
            subscriptions: state
                .subscriptions
                .iter()
                .map(|url| Subscription {
                    channel: SerializedChannel::from_url(url.clone()),
                    creation_time: now,
                })
                .collect(),
            subscription_removals: BTreeMap::new(),
        };
        serde_json::to_string(&pkg)
            .map_err(|e| SyncError::Transport(format!("encode subscriptions: {e}")))
    }
}

impl SyncTransport for GrayjaySyncTransport {
    fn pair(&mut self, pairing_blob: &str) -> SyncResult<PeerId> {
        let mut info = Self::parse_blob(pairing_blob)?;
        let pairing_code = Zeroizing::new(info.pairing_code.take().unwrap_or_default());
        let session = self.open_session(
            &info,
            (!pairing_code.is_empty()).then_some(pairing_code.as_str()),
        )?;
        let peer_key = session.remote_public_key().to_string();
        let mut remembered = info.clone();
        remembered.public_key.clone_from(&peer_key);
        remembered.pairing_code = None;
        self.peers.insert(peer_key.clone(), remembered);
        Ok(PeerId(peer_key))
    }

    fn push(&mut self, peer: &PeerId, state: &SyncState) -> SyncResult<()> {
        let info = self
            .peers
            .get(&peer.0)
            .cloned()
            .ok_or_else(|| SyncError::Pairing(format!("unknown peer {}", peer.0)))?;
        let mut session = self.open_session(&info, None)?;
        let json = Self::subscriptions_json(state)?;
        session.send_data(sync_opcode::SYNC_SUBSCRIPTIONS, &json)?;
        Ok(())
    }

    fn pull(&mut self, peer: &PeerId) -> SyncResult<SyncState> {
        let info = self
            .peers
            .get(&peer.0)
            .cloned()
            .ok_or_else(|| SyncError::Pairing(format!("unknown peer {}", peer.0)))?;
        let mut session = self.open_session(&info, None)?;

        let req = SyncSessionData::request_all(self.device.public_base64());
        let req_json = serde_json::to_string(&req)
            .map_err(|e| SyncError::Transport(format!("encode state-exchange: {e}")))?;
        session.send_data(sync_opcode::SYNC_STATE_EXCHANGE, &req_json)?;

        let mut out = SyncState::default();
        let deadline = ui::time::now() + self.sync_deadline;
        let mut got_subs = false;
        while ui::time::now() < deadline {
            match session.recv() {
                Ok(Incoming::Data {
                    sub_opcode,
                    payload,
                }) => match sub_opcode {
                    sync_opcode::SYNC_SUBSCRIPTIONS => {
                        if let Ok(pkg) =
                            serde_json::from_slice::<SyncSubscriptionsPackage>(&payload)
                        {
                            out.subscriptions = pkg
                                .subscriptions
                                .into_iter()
                                .map(|s| s.channel.url)
                                .collect();
                            got_subs = true;
                        }
                    }
                    sync_opcode::SYNC_HISTORY => {
                        out.watch_history = parse_history_urls(&payload);
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(_) => break,
            }
            if got_subs && ui::time::now() + Duration::from_millis(500) >= deadline {
                break;
            }
        }
        Ok(out)
    }

    fn restore_peer(&mut self, peer: &PeerDescriptor) -> SyncResult<()> {
        let info = SyncDeviceInfo {
            public_key: peer.public_key.clone(),
            addresses: peer.addresses.clone(),
            port: peer.port,
            pairing_code: None,
        };
        validate_device_info(&info).map_err(|_| {
            SyncError::Persistence("stored peer connection metadata is invalid".into())
        })?;
        self.remember_peer(info);
        Ok(())
    }

    fn forget_peer(&mut self, peer: &PeerId) {
        self.peers.remove(&peer.0);
    }

    fn peer_descriptor(&self, peer: &PeerId) -> Option<PeerDescriptor> {
        self.peers.get(&peer.0).map(|info| PeerDescriptor {
            public_key: info.public_key.clone(),
            addresses: info.addresses.clone(),
            port: info.port,
        })
    }
}

fn parse_history_urls(payload: &[u8]) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Vid {
        #[serde(default)]
        url: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Hist {
        #[serde(default)]
        video: Option<Vid>,
    }
    serde_json::from_slice::<Vec<Hist>>(payload)
        .map(|list| {
            list.into_iter()
                .filter_map(|h| h.video.and_then(|v| v.url))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn noise_transport_frames_pairs_and_streams_payloads() {
        {
            let mut wire = Vec::new();
            write_frame(&mut wire, b"noise").unwrap();
            assert_eq!(wire, [5, 0, 0, 0, b'n', b'o', b'i', b's', b'e']);
            assert_eq!(read_frame(&mut wire.as_slice()).unwrap(), b"noise");
            assert!(read_frame(&mut [0_u8, 0, 1, 0].as_slice()).is_err());
            assert!(write_frame(&mut Vec::new(), &vec![0; 65_536]).is_err());
        }
        {
            let responder_key = DeviceKey::generate().unwrap();
            let initiator_key = DeviceKey::generate().unwrap();
            let responder_pub = responder_key.public_base64();
            let initiator_pub = initiator_key.public_base64();

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let rkey = responder_key;
            let expect_initiator = initiator_pub;
            let (frame_read, read_back) = std::sync::mpsc::channel::<()>();
            let handle = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                let mut session = GrayjaySession::accept_responder(
                    stream,
                    &rkey,
                    "responder",
                    |peer_key, _code| peer_key == expect_initiator,
                )
                .unwrap();

                let deadline = tilo_test_env::hang_deadline();
                session.complete_authorization(deadline).unwrap();

                let mut asked = false;
                for _ in 0..8 {
                    if let Incoming::Data {
                        sub_opcode,
                        payload,
                    } = session.recv().unwrap()
                        && sub_opcode == sync_opcode::SYNC_STATE_EXCHANGE
                    {
                        let _req: SyncSessionData = serde_json::from_slice(&payload).unwrap();
                        asked = true;
                        break;
                    }
                }
                assert!(asked, "responder never received the state-exchange request");

                let pkg = SyncSubscriptionsPackage {
                    subscriptions: vec![
                        Subscription {
                            channel: SerializedChannel::from_url("https://youtube.com/@alice"),
                            creation_time: 111,
                        },
                        Subscription {
                            channel: SerializedChannel::from_url("https://youtube.com/@bob"),
                            creation_time: 222,
                        },
                    ],
                    subscription_removals: BTreeMap::new(),
                };
                let json = serde_json::to_string(&pkg).unwrap();
                session
                    .send_data(sync_opcode::SYNC_SUBSCRIPTIONS, &json)
                    .unwrap();
                let _ = read_back.recv_timeout(tilo_test_env::hang_bound());
            });

            let stream = TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let mut session = GrayjaySession::connect_initiator(
                stream,
                &initiator_key,
                &responder_pub,
                None,
                "initiator",
            )
            .unwrap();
            assert_eq!(session.remote_public_key(), responder_pub);
            assert_eq!(session.remote_version(), PROTOCOL_VERSION);

            let deadline = tilo_test_env::hang_deadline();
            let name = session.complete_authorization(deadline).unwrap();
            assert_eq!(name, "responder");

            request_all_subscriptions(&mut session, &initiator_key);

            let mut urls = Vec::new();
            for _ in 0..8 {
                if let Incoming::Data {
                    sub_opcode,
                    payload,
                } = session.recv().unwrap()
                    && sub_opcode == sync_opcode::SYNC_SUBSCRIPTIONS
                {
                    let pkg: SyncSubscriptionsPackage = serde_json::from_slice(&payload).unwrap();
                    urls = pkg
                        .subscriptions
                        .into_iter()
                        .map(|s| s.channel.url)
                        .collect();
                    break;
                }
            }
            drop(frame_read);
            handle.join().unwrap();
            assert_eq!(
                urls,
                vec![
                    "https://youtube.com/@alice".to_string(),
                    "https://youtube.com/@bob".to_string()
                ]
            );
        }
        {
            let responder_key = DeviceKey::generate().unwrap();
            let initiator_key = DeviceKey::generate().unwrap();
            let responder_pub = responder_key.public_base64();

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let rkey = responder_key;
            let handle = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                let mut session =
                    GrayjaySession::accept_responder(stream, &rkey, "responder", |_peer, code| {
                        code == Some("SECRET42")
                    })
                    .unwrap();
                let deadline = tilo_test_env::hang_deadline();
                session.complete_authorization(deadline).unwrap();
            });

            let stream = TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let mut session = GrayjaySession::connect_initiator(
                stream,
                &initiator_key,
                &responder_pub,
                Some("SECRET42"),
                "initiator",
            )
            .unwrap();
            let deadline = tilo_test_env::hang_deadline();
            session.complete_authorization(deadline).unwrap();
            handle.join().unwrap();
        }
        {
            let responder_key = DeviceKey::generate().unwrap();
            let initiator_key = DeviceKey::generate().unwrap();
            let responder_pub = responder_key.public_base64();
            let initiator_pub = initiator_key.public_base64();

            let mut subs = Vec::new();
            for i in 0..4000 {
                subs.push(Subscription {
                    channel: SerializedChannel::from_url(format!(
                        "https://youtube.com/channel/UC{i:08}xxxxxxxx"
                    )),
                    creation_time: i64::from(i),
                });
            }
            let pkg = SyncSubscriptionsPackage {
                subscriptions: subs,
                subscription_removals: BTreeMap::new(),
            };
            let json = serde_json::to_string(&pkg).unwrap();
            assert!(
                json.len() > MAXIMUM_PACKET_SIZE,
                "test payload must exceed one packet"
            );
            let expected_len = pkg.subscriptions.len();

            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();

            let rkey = responder_key;
            let expect_initiator = initiator_pub;
            let json_for_thread = json;
            let (frame_read, read_back) = std::sync::mpsc::channel::<()>();
            let handle = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                let mut session =
                    GrayjaySession::accept_responder(stream, &rkey, "responder", |peer, _code| {
                        peer == expect_initiator
                    })
                    .unwrap();
                let deadline = tilo_test_env::hang_deadline();
                session.complete_authorization(deadline).unwrap();
                session
                    .send_data(sync_opcode::SYNC_SUBSCRIPTIONS, &json_for_thread)
                    .unwrap();
                let _ = read_back.recv_timeout(tilo_test_env::hang_bound());
            });

            let stream = TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let mut session = GrayjaySession::connect_initiator(
                stream,
                &initiator_key,
                &responder_pub,
                None,
                "initiator",
            )
            .unwrap();
            let deadline = tilo_test_env::hang_deadline();
            session.complete_authorization(deadline).unwrap();

            let mut got = 0usize;
            for _ in 0..4 {
                if let Incoming::Data {
                    sub_opcode,
                    payload,
                } = session.recv().unwrap()
                    && sub_opcode == sync_opcode::SYNC_SUBSCRIPTIONS
                {
                    let received: SyncSubscriptionsPackage =
                        serde_json::from_slice(&payload).unwrap();
                    got = received.subscriptions.len();
                    break;
                }
            }
            drop(frame_read);
            handle.join().unwrap();
            assert_eq!(got, expected_len);
        }
    }

    fn request_all_subscriptions(session: &mut GrayjaySession, device: &DeviceKey) {
        let request = SyncSessionData::request_all(device.public_base64());
        let json = serde_json::to_string(&request).unwrap();
        session
            .send_data(sync_opcode::SYNC_STATE_EXCHANGE, &json)
            .unwrap();
    }

    #[test]
    fn transport_payloads_parse_and_stay_bounded() {
        {
            let json = serde_json::to_string(&SyncDeviceInfo {
                public_key: b64_encode(&[3; 32]),
                addresses: vec!["10.0.0.2".into()],
                port: 12315,
                pairing_code: Some("Xy".into()),
            })
            .unwrap();
            let a = GrayjaySyncTransport::parse_blob(&json).unwrap();
            assert_eq!(a.port, 12315);

            let wrapped = format!("grayjay://sync/{}", b64_encode(json.as_bytes()));
            let b = GrayjaySyncTransport::parse_blob(&wrapped).unwrap();
            assert_eq!(a, b);
        }
        {
            assert!(
                GrayjaySyncTransport::parse_blob(&"x".repeat(MAX_PAIRING_BLOB_BYTES + 1)).is_err()
            );
            let too_many_addresses = serde_json::to_string(&SyncDeviceInfo {
                public_key: b64_encode(&[4; 32]),
                addresses: vec!["127.0.0.1".into(); MAX_PEER_ADDRESSES + 1],
                port: 12315,
                pairing_code: Some("ok".into()),
            })
            .unwrap();
            assert!(GrayjaySyncTransport::parse_blob(&too_many_addresses).is_err());
        }
        {
            let compressed = gzip_compress(&vec![0; MAX_SYNC_PAYLOAD_BYTES + 1]).unwrap();
            let error = gzip_decompress(&compressed).unwrap_err();
            assert!(error.to_string().contains("exceeds resource bound"));
        }
    }
}
