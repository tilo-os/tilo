//! Grayjay's Sync transport: the Noise Protocol Framework over TCP.
//!
//! A Grayjay device listens on TCP 12315 and advertises itself as a pairing invite (a QR
//! code, a link, or the same JSON pasted by hand). This client connects to it as the Noise
//! initiator, completes Grayjay's version check, handshake and authorization, and then
//! exchanges the two packages Grayjay's Sync screen carries: the subscriptions the person
//! follows, and the watch history the other app recorded. Every wire detail is a
//! clean-room reimplementation of the facts documented and cited in [`crate::protocol`].
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
//!
//! Sync is read-only in one direction: we ask for everything the peer holds
//! ([`SyncSessionData::request_all`]) and read its subscriptions and history, then send our own
//! subscriptions. A pairing code never leaves the invite it came in: it is held in memory for
//! the one handshake, and the peer route the app records afterwards has none.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use base64::Engine;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use snow::params::DHChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use snow::{Builder, HandshakeState, TransportState};
use tilo_clock::unix_now_i64;
use zeroize::Zeroizing;

use crate::protocol::{
    self, Frame, HEADER_SIZE, MAXIMUM_PACKET_SIZE, MAXIMUM_PACKET_SIZE_ENCRYPTED,
    MINIMUM_PROTOCOL_VERSION, NOISE_PATTERN_CODE, NOISE_PATTERN_MAIN, PROTOCOL_VERSION,
    SerializedChannel, Subscription, SyncDeviceInfo, SyncSessionData, SyncSubscriptionsPackage,
    content_encoding, notify, opcode, stream, sync_opcode,
};
use crate::{Error, PeerRoute, Pulled, SharedSubscription};

/// How long connecting, one read or write, and one whole exchange may take.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    pub connect: Duration,
    pub io: Duration,
    pub exchange: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            io: Duration::from_secs(15),
            exchange: Duration::from_secs(20),
        }
    }
}

/// A subscription/history exchange is metadata, not a media transfer. This cap bounds
/// streamed reassembly, gzip expansion, and JSON allocation even if an authenticated peer
/// is buggy or hostile.
const MAX_SYNC_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
/// Once a peer's subscriptions are in, the history beside them takes a moment more: a peer that
/// says nothing in this window has said everything it has.
const GRACE: Duration = Duration::from_millis(500);
const MAX_CONCURRENT_STREAMS: usize = 4;
const MAX_INVITE_BYTES: usize = 16 * 1024;
const MAX_PEER_ADDRESSES: usize = 8;
const MAX_PEER_ADDRESS_BYTES: usize = 255;
const MAX_CODE_BYTES: usize = 1024;

fn b64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn b64_decode(s: &str) -> Result<Vec<u8>, Error> {
    let s = s.trim();
    for engine in [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ] {
        if let Ok(value) = engine.decode(s) {
            return Ok(value);
        }
    }
    Err(Error::Pairing("invalid base64 value".into()))
}

/// Check the pairing metadata Grayjay advertises before a connection is opened.
fn validate_device_info(info: &SyncDeviceInfo) -> Result<(), Error> {
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
            .code
            .as_ref()
            .is_some_and(|code| code.len() > MAX_CODE_BYTES)
    {
        return Err(Error::Pairing(
            "the invite is malformed or exceeds its bounds".into(),
        ));
    }
    Ok(())
}

fn snow_err(error: &snow::Error) -> Error {
    Error::Transport(format!("noise: {error}"))
}

fn io_err(error: &io::Error) -> Error {
    Error::Transport(format!("io: {error}"))
}

/// The device's Noise static key, derived from the key the device holds for the app. Grayjay identifies a device by this key: it is what the peer shows as the
/// fingerprint the person checks on both ends.
#[derive(Clone)]
pub struct DeviceKey {
    private: Zeroizing<Vec<u8>>,
    pub public: Vec<u8>,
}

impl DeviceKey {
    /// The key of a 32-byte seed, as the private half of a Curve25519 pair.
    pub fn from_private(private: &[u8]) -> Result<Self, Error> {
        if private.len() != 32 {
            return Err(Error::KeyUnavailable(
                "device key material has an invalid length".into(),
            ));
        }
        let mut dh = DefaultResolver
            .resolve_dh(&DHChoice::Curve25519)
            .ok_or_else(|| Error::KeyUnavailable("Curve25519 is unavailable".into()))?;
        dh.set(private);
        Ok(Self {
            private: Zeroizing::new(private.to_vec()),
            public: dh.pubkey().to_vec(),
        })
    }

    /// The key as the base64 Grayjay names it: the fingerprint a person compares.
    pub fn public_base64(&self) -> String {
        b64_encode(&self.public)
    }
}

/// One read of an authorized session: a data package, or the peer's word about authorization.
#[derive(Clone, Debug)]
pub enum Incoming {
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

/// One authorized Grayjay session: the encrypted channel, and what it has assembled.
pub struct Session {
    stream: TcpStream,
    transport: TransportState,
    remote_public_key: String,
    remote_version: i32,
    remote_authorized: bool,
    streams: HashMap<i32, Reasm>,
    next_stream_id: i32,
    device_name: String,
}

impl Session {
    #[must_use]
    pub fn remote_public_key(&self) -> &str {
        &self.remote_public_key
    }

    #[must_use]
    pub fn remote_version(&self) -> i32 {
        self.remote_version
    }

    fn version_check(stream: &mut TcpStream) -> Result<i32, Error> {
        stream
            .write_all(&PROTOCOL_VERSION.to_le_bytes())
            .map_err(|error| io_err(&error))?;
        let mut buf = [0u8; 4];
        read_exact(stream, &mut buf)?;
        let remote = i32::from_le_bytes(buf);
        if remote < MINIMUM_PROTOCOL_VERSION {
            return Err(Error::Transport(format!(
                "peer protocol version {remote} < minimum {MINIMUM_PROTOCOL_VERSION}"
            )));
        }
        Ok(remote)
    }

    /// Connect to a Grayjay device that already knows our static key, carrying the invite's
    /// code when this connection is the one that adds it.
    pub fn connect_initiator(
        mut stream: TcpStream,
        device: &DeviceKey,
        remote_public_key_b64: &str,
        code: Option<&str>,
        device_name: &str,
    ) -> Result<Self, Error> {
        let remote_pub = b64_decode(remote_public_key_b64)?;
        let remote_version = Self::version_check(&mut stream)?;

        let params = NOISE_PATTERN_MAIN
            .parse()
            .map_err(|error| snow_err(&error))?;
        let mut hs: HandshakeState = Builder::new(params)
            .local_private_key(&device.private)
            .remote_public_key(&remote_pub)
            .build_initiator()
            .map_err(|error| snow_err(&error))?;

        let code_msg: Vec<u8> = if let Some(code) = code {
            let code_params = NOISE_PATTERN_CODE
                .parse()
                .map_err(|error| snow_err(&error))?;
            let mut handshake: HandshakeState = Builder::new(code_params)
                .remote_public_key(&remote_pub)
                .build_initiator()
                .map_err(|error| snow_err(&error))?;
            let mut buf = vec![0u8; 512];
            let written = handshake
                .write_message(code.as_bytes(), &mut buf)
                .map_err(|error| snow_err(&error))?;
            buf.truncate(written);
            buf
        } else {
            Vec::new()
        };

        let mut main_buf = vec![0u8; 512];
        let main_len = hs
            .write_message(&[], &mut main_buf)
            .map_err(|error| snow_err(&error))?;

        let app_id: u32 = 0;
        let mut message = Vec::with_capacity(8 + code_msg.len() + main_len);
        message.extend_from_slice(&app_id.to_le_bytes());
        let code_len = u32::try_from(code_msg.len())
            .map_err(|_| Error::Transport("the invite's code is too long".into()))?;
        message.extend_from_slice(&code_len.to_le_bytes());
        message.extend_from_slice(&code_msg);
        let main_message = main_buf
            .get(..main_len)
            .ok_or_else(|| Error::Transport("Noise produced an invalid handshake length".into()))?;
        message.extend_from_slice(main_message);
        write_frame(&mut stream, &message)?;

        let response = read_frame(&mut stream)?;
        let mut scratch = vec![0u8; 512];
        hs.read_message(&response, &mut scratch)
            .map_err(|error| snow_err(&error))?;

        // Capture and verify the peer's static key before consuming the handshake.
        let remote_static = hs
            .get_remote_static()
            .map(<[u8]>::to_vec)
            .ok_or_else(|| Error::Transport("peer did not present a static key".into()))?;
        let remote_static_b64 = b64_encode(&remote_static);
        if remote_static_b64 != b64_encode(&remote_pub) {
            return Err(Error::Pairing(format!(
                "peer key mismatch: expected {remote_public_key_b64}, got {remote_static_b64}"
            )));
        }

        let transport = hs.into_transport_mode().map_err(|error| snow_err(&error))?;

        Ok(Self {
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

    /// Accept a Grayjay client's connection: the other half of [`Self::connect_initiator`],
    /// for a device that listens. `allow` decides whether the peer may connect, given its
    /// static key and the pairing code it carried.
    pub fn accept_responder(
        mut stream: TcpStream,
        device: &DeviceKey,
        device_name: &str,
        allow: impl Fn(&str, Option<&str>) -> bool,
    ) -> Result<Self, Error> {
        let remote_version = Self::version_check(&mut stream)?;

        let params = NOISE_PATTERN_MAIN
            .parse()
            .map_err(|error| snow_err(&error))?;
        let mut hs: HandshakeState = Builder::new(params)
            .local_private_key(&device.private)
            .build_responder()
            .map_err(|error| snow_err(&error))?;

        let message = read_frame(&mut stream)?;
        let header = message
            .get(..8)
            .ok_or_else(|| Error::Transport("handshake frame too short".into()))?;
        let Some(&[a0, a1, a2, a3, l0, l1, l2, l3]) = header.first_chunk::<8>() else {
            return Err(Error::Transport("handshake frame too short".into()));
        };
        let _app_id = u32::from_le_bytes([a0, a1, a2, a3]);
        let code_len = usize::try_from(u32::from_le_bytes([l0, l1, l2, l3]))
            .map_err(|_| Error::Transport("the invite's code length overflows the frame".into()))?;
        let rest = message
            .get(8..)
            .ok_or_else(|| Error::Transport("handshake frame too short".into()))?;
        let (code_msg, main_msg) = rest.split_at(code_len);

        let code: Option<String> = if code_len > 0 {
            let code_params = NOISE_PATTERN_CODE
                .parse()
                .map_err(|error| snow_err(&error))?;
            let mut handshake: HandshakeState = Builder::new(code_params)
                .local_private_key(&device.private)
                .build_responder()
                .map_err(|error| snow_err(&error))?;
            let mut plaintext = vec![0u8; 512];
            let read = handshake
                .read_message(code_msg, &mut plaintext)
                .map_err(|error| snow_err(&error))?;
            let decoded = plaintext
                .get(..read)
                .ok_or_else(|| Error::Transport("Noise produced an invalid code length".into()))?;
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
            .ok_or_else(|| Error::Transport("initiator did not present a static key".into()))?;
        let remote_static_b64 = b64_encode(&remote_static);
        if remote_static_b64 == device.public_base64()
            || !allow(&remote_static_b64, code.as_deref())
        {
            return Err(Error::Pairing("handshake not allowed".into()));
        }

        let mut response_buffer = vec![0u8; 512];
        let written = hs
            .write_message(&[], &mut response_buffer)
            .map_err(|error| snow_err(&error))?;
        let response = response_buffer
            .get(..written)
            .ok_or_else(|| Error::Transport("Noise produced an invalid handshake length".into()))?;
        write_frame(&mut stream, response)?;

        let transport = hs.into_transport_mode().map_err(|error| snow_err(&error))?;

        Ok(Self {
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

    fn write_encrypted(&mut self, plaintext: &[u8]) -> Result<(), Error> {
        let mut cipher = vec![0u8; plaintext.len().saturating_add(16)];
        let len = self
            .transport
            .write_message(plaintext, &mut cipher)
            .map_err(|error| snow_err(&error))?;
        let encrypted = cipher.get(..len).ok_or_else(|| {
            Error::Transport("Noise produced an invalid ciphertext length".into())
        })?;
        write_frame(&mut self.stream, encrypted)
    }

    fn read_encrypted(&mut self) -> Result<Vec<u8>, Error> {
        let cipher = read_frame(&mut self.stream)?;
        let mut plain = vec![0u8; cipher.len()];
        let len = self
            .transport
            .read_message(&cipher, &mut plain)
            .map_err(|error| snow_err(&error))?;
        plain.truncate(len);
        Ok(plain)
    }

    fn send(
        &mut self,
        opcode: u8,
        sub_opcode: u8,
        payload: &[u8],
        gzip: bool,
    ) -> Result<(), Error> {
        if payload.len() > MAX_SYNC_PAYLOAD_BYTES {
            return Err(Error::Transport(
                "outgoing sync payload exceeds its resource bound".into(),
            ));
        }
        let (encoding, processed) = if gzip && opcode == opcode::DATA {
            (content_encoding::GZIP, gzip_compress(payload)?)
        } else {
            (content_encoding::RAW, payload.to_vec())
        };
        if processed.len() > MAX_SYNC_PAYLOAD_BYTES {
            return Err(Error::Transport(
                "encoded sync payload exceeds its resource bound".into(),
            ));
        }

        if processed.len().saturating_add(HEADER_SIZE) <= MAXIMUM_PACKET_SIZE {
            let plaintext = protocol::encode_frame(opcode, sub_opcode, encoding, &processed);
            return self.write_encrypted(&plaintext);
        }

        let id = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        let segment_size = MAXIMUM_PACKET_SIZE.saturating_sub(HEADER_SIZE);
        let total = processed.len();
        let mut offset = 0usize;
        while offset < total {
            let remaining = total.saturating_sub(offset);
            let mut segment = Vec::with_capacity(segment_size);
            let (stream_opcode, bytes_to_send) = if offset == 0 {
                let sending = segment_size.saturating_sub(4 + HEADER_SIZE);
                segment.extend_from_slice(&id.to_le_bytes());
                let total = u32::try_from(total)
                    .map_err(|_| Error::Transport("the payload is too large to stream".into()))?;
                segment.extend_from_slice(&total.to_le_bytes());
                segment.push(opcode);
                segment.push(sub_opcode);
                segment.push(encoding);
                (stream::START, sending)
            } else {
                let sending = segment_size.saturating_sub(4 + 4).min(remaining);
                let opcode = if sending >= remaining {
                    stream::END
                } else {
                    stream::DATA
                };
                segment.extend_from_slice(&id.to_le_bytes());
                let at = u32::try_from(offset)
                    .map_err(|_| Error::Transport("the payload is too large to stream".into()))?;
                segment.extend_from_slice(&at.to_le_bytes());
                (opcode, sending)
            };
            let end = offset.saturating_add(bytes_to_send);
            let chunk = processed
                .get(offset..end)
                .ok_or_else(|| Error::Transport("stream segment bounds are invalid".into()))?;
            segment.extend_from_slice(chunk);
            let plaintext = protocol::encode_frame(
                opcode::STREAM,
                stream_opcode,
                content_encoding::RAW,
                &segment,
            );
            self.write_encrypted(&plaintext)?;
            offset = end;
        }
        Ok(())
    }

    /// Send one JSON package as a DATA frame, gzip'd as Grayjay sends them.
    pub fn send_data(&mut self, sub_opcode: u8, json: &str) -> Result<(), Error> {
        self.send(opcode::DATA, sub_opcode, json.as_bytes(), true)
    }

    /// Send this device's name, the authorization Grayjay's session opens with.
    pub fn send_authorized(&mut self) -> Result<(), Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let name = self.device_name.clone();
        let id_bytes = id.as_bytes();
        let name_bytes = name.as_bytes();
        let mut payload = Vec::with_capacity(2 + id_bytes.len() + name_bytes.len());
        payload.push(u8::try_from(id_bytes.len()).unwrap_or(u8::MAX));
        payload.extend_from_slice(id_bytes);
        payload.push(u8::try_from(name_bytes.len()).unwrap_or(u8::MAX));
        payload.extend_from_slice(name_bytes);
        self.send(opcode::NOTIFY, notify::AUTHORIZED, &payload, false)
    }

    fn send_pong(&mut self) -> Result<(), Error> {
        self.send(opcode::PONG, 0, &[], false)
    }

    /// How long the next read waits before it gives up: a pull that has what it asked for
    /// waits only a moment more for what the peer sends with it.
    pub fn set_read_timeout(&self, timeout: Duration) -> Result<(), Error> {
        self.stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| io_err(&error))
    }

    /// Read one package or authorization word, answering pings and reassembling streams.
    pub fn recv(&mut self) -> Result<Incoming, Error> {
        loop {
            let plain = self.read_encrypted()?;
            let frame = protocol::decode_frame(&plain)
                .map_err(|error| Error::Transport(format!("frame decode: {error}")))?;

            match frame.opcode {
                opcode::PING => {
                    self.send_pong()?;
                }
                opcode::STREAM => {
                    if let Some(resolved) = self.handle_stream(&frame)?
                        && let Some(incoming) = Self::resolve(resolved)?
                    {
                        return Ok(incoming);
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
                    if let Some(incoming) = Self::resolve(frame)? {
                        return Ok(incoming);
                    }
                }
                _ => {}
            }
        }
    }

    fn resolve(frame: Frame) -> Result<Option<Incoming>, Error> {
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

    fn handle_stream(&mut self, frame: &Frame) -> Result<Option<Frame>, Error> {
        let payload = frame.payload.as_slice();
        let short = || Error::Transport("stream segment too short".into());
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
                    return Err(Error::Transport(
                        "stream declaration exceeds its resource bounds".into(),
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
                let (id, expected_offset, chunk) = stream_header(payload)?;
                let reassembly = self
                    .streams
                    .get_mut(&id)
                    .ok_or_else(|| Error::Transport("stream data for an unknown id".into()))?;
                if expected_offset != reassembly.buf.len() {
                    return Err(Error::Transport("stream offset mismatch".into()));
                }
                if chunk.len()
                    > reassembly
                        .expected_size
                        .saturating_sub(reassembly.buf.len())
                {
                    return Err(Error::Transport(
                        "stream data exceeds its declared size".into(),
                    ));
                }
                reassembly.buf.extend_from_slice(chunk);
                Ok(None)
            }
            stream::END => {
                let (id, expected_offset, chunk) = stream_header(payload)?;
                let mut reassembly = self
                    .streams
                    .remove(&id)
                    .ok_or_else(|| Error::Transport("stream end for an unknown id".into()))?;
                if expected_offset != reassembly.buf.len() {
                    return Err(Error::Transport("stream offset mismatch at end".into()));
                }
                if chunk.len()
                    > reassembly
                        .expected_size
                        .saturating_sub(reassembly.buf.len())
                {
                    return Err(Error::Transport(
                        "stream end exceeds its declared size".into(),
                    ));
                }
                reassembly.buf.extend_from_slice(chunk);
                if reassembly.buf.len() != reassembly.expected_size {
                    return Err(Error::Transport("reassembled stream size mismatch".into()));
                }
                Ok(Some(Frame {
                    opcode: reassembly.opcode,
                    sub_opcode: reassembly.sub_opcode,
                    content_encoding: reassembly.content_encoding,
                    payload: reassembly.buf,
                }))
            }
            _ => Ok(None),
        }
    }

    /// Send our name and wait for the peer's authorization, telling us its name.
    pub fn complete_authorization(&mut self, deadline: Instant) -> Result<String, Error> {
        self.send_authorized()?;
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Transport(
                    "timed out waiting for the peer's authorization".into(),
                ));
            }
            match self.recv()? {
                Incoming::Authorized { remote_name } => return Ok(remote_name),
                Incoming::Unauthorized => {
                    return Err(Error::Pairing("the peer refused authorization".into()));
                }
                Incoming::Data { .. } => {}
            }
        }
    }
}

fn stream_header(payload: &[u8]) -> Result<(i32, usize, &[u8]), Error> {
    let header = payload
        .get(..8)
        .ok_or_else(|| Error::Transport("stream segment too short".into()))?;
    let Some(&[i0, i1, i2, i3, o0, o1, o2, o3]) = header.first_chunk::<8>() else {
        return Err(Error::Transport("stream segment too short".into()));
    };
    let body = payload
        .get(8..)
        .ok_or_else(|| Error::Transport("stream segment too short".into()))?;
    let offset = u32::from_le_bytes([o0, o1, o2, o3]);
    Ok((
        i32::from_le_bytes([i0, i1, i2, i3]),
        usize::try_from(offset).unwrap_or(usize::MAX),
        body,
    ))
}

/// Every Grayjay sync packet: a little-endian `u32` length, then the Noise handshake message
/// or ciphertext.
const FRAMING: wire_frame::Framing =
    wire_frame::Framing::little_endian(MAXIMUM_PACKET_SIZE_ENCRYPTED);

fn write_frame(stream: &mut TcpStream, payload: &[u8]) -> Result<(), Error> {
    FRAMING
        .write(stream, payload)
        .map_err(|error| Error::Transport(error.to_string()))
}

fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>, Error> {
    match FRAMING.read(stream) {
        Ok(Some(frame)) => Ok(frame),
        Ok(None) => Err(io_err(&io::ErrorKind::UnexpectedEof.into())),
        Err(wire_frame::FrameError::TooLarge(len)) => {
            Err(Error::Transport(format!("frame too large: {len}")))
        }
        Err(error) => Err(io_err(&error.into())),
    }
}

fn read_exact(stream: &mut TcpStream, buf: &mut [u8]) -> Result<(), Error> {
    stream.read_exact(buf).map_err(|error| io_err(&error))
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

fn gzip_compress(data: &[u8]) -> Result<Vec<u8>, Error> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).map_err(|error| io_err(&error))?;
    encoder.finish().map_err(|error| io_err(&error))
}

fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, Error> {
    let mut decoder = GzDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .by_ref()
        .take(
            u64::try_from(MAX_SYNC_PAYLOAD_BYTES)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut out)
        .map_err(|error| io_err(&error))?;
    if out.len() > MAX_SYNC_PAYLOAD_BYTES {
        return Err(Error::Transport(
            "decompressed sync payload exceeds its resource bound".into(),
        ));
    }
    Ok(out)
}

/// A Grayjay device's invite, as its Sync screen shows it: the JSON
/// [`SyncDeviceInfo`], or the same JSON base64'd at the end of a link.
pub fn parse_invite(blob: &str) -> Result<SyncDeviceInfo, Error> {
    let blob = blob.trim();
    if blob.is_empty() || blob.len() > MAX_INVITE_BYTES {
        return Err(Error::Pairing("the invite is empty or too large".into()));
    }
    if let Ok(info) = serde_json::from_str::<SyncDeviceInfo>(blob) {
        validate_device_info(&info)?;
        return Ok(info);
    }
    let payload = blob.rsplit(['/', ':']).next().unwrap_or(blob);
    let bytes = b64_decode(payload)?;
    if bytes.len() > MAX_INVITE_BYTES {
        return Err(Error::Pairing("the decoded invite is too large".into()));
    }
    let info = serde_json::from_slice::<SyncDeviceInfo>(&bytes)
        .map_err(|error| Error::Pairing(format!("unrecognized invite: {error}")))?;
    validate_device_info(&info)?;
    Ok(info)
}

/// Our side of Grayjay's Sync: connects to a device, pairs with it, and exchanges packages.
pub struct SyncClient {
    device: DeviceKey,
    device_name: String,
    timeouts: Timeouts,
}

impl SyncClient {
    #[must_use]
    pub fn new(device: DeviceKey, device_name: impl Into<String>) -> Self {
        Self {
            device,
            device_name: device_name.into(),
            timeouts: Timeouts::default(),
        }
    }

    #[must_use]
    pub fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    #[must_use]
    pub fn public_key(&self) -> String {
        self.device.public_base64()
    }

    fn connect(&self, host: &str, port: u16) -> Result<TcpStream, Error> {
        let mut last = Error::Transport(format!("no address to reach {host} at"));
        for address in (host, port)
            .to_socket_addrs()
            .map_err(|error| io_err(&error))?
        {
            match TcpStream::connect_timeout(&address, self.timeouts.connect) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(self.timeouts.io))
                        .map_err(|error| io_err(&error))?;
                    stream
                        .set_write_timeout(Some(self.timeouts.io))
                        .map_err(|error| io_err(&error))?;
                    return Ok(stream);
                }
                Err(error) => last = io_err(&error),
            }
        }
        Err(last)
    }

    fn open_session(&self, info: &SyncDeviceInfo, code: Option<&str>) -> Result<Session, Error> {
        let port = u16::try_from(info.port)
            .map_err(|_| Error::Transport(format!("invalid port {}", info.port)))?;
        let mut last = Error::Transport("the invite names no address".into());
        for address in &info.addresses {
            let stream = match self.connect(address, port) {
                Ok(stream) => stream,
                Err(error) => {
                    last = error;
                    continue;
                }
            };
            let mut session = Session::connect_initiator(
                stream,
                &self.device,
                &info.public_key,
                code.or(info.code.as_deref()),
                &self.device_name,
            )?;
            let deadline = Instant::now() + self.timeouts.exchange;
            session.complete_authorization(deadline)?;
            return Ok(session);
        }
        Err(last)
    }

    /// Add the device the invite names: the handshake Grayjay shows as a fingerprint the person
    /// approves on the other end. The route it answers is what the app records.
    pub fn pair(&self, invite: &str) -> Result<PeerRoute, Error> {
        let mut info = parse_invite(invite)?;
        let code = Zeroizing::new(info.code.take().unwrap_or_default());
        let session = self.open_session(&info, (!code.is_empty()).then_some(code.as_str()))?;
        let peer_key = session.remote_public_key().to_string();
        Ok(PeerRoute {
            name: format!("Grayjay {}", peer_key.chars().take(8).collect::<String>()),
            key: peer_key,
            addresses: info.addresses,
            port: info.port,
        })
    }

    /// Exchange with one paired device: read everything it holds, then send our subscriptions.
    pub fn sync(
        &self,
        peer: &PeerRoute,
        subscriptions: &[SharedSubscription],
    ) -> Result<Pulled, Error> {
        let info = SyncDeviceInfo {
            public_key: peer.key.clone(),
            addresses: peer.addresses.clone(),
            port: peer.port,
            code: None,
        };
        validate_device_info(&info)
            .map_err(|_| Error::Persistence("the stored device's route is invalid".into()))?;
        let mut session = self.open_session(&info, None)?;

        let request = SyncSessionData::request_all(self.device.public_base64());
        let json = serde_json::to_string(&request)
            .map_err(|error| Error::Transport(format!("encode state-exchange: {error}")))?;
        session.send_data(sync_opcode::SYNC_STATE_EXCHANGE, &json)?;

        let mut pulled = Pulled::default();
        let deadline = Instant::now() + self.timeouts.exchange;
        let mut quiet_until: Option<Instant> = None;
        while Instant::now() < deadline {
            match session.recv() {
                Ok(Incoming::Data {
                    sub_opcode,
                    payload,
                }) => match sub_opcode {
                    sync_opcode::SYNC_SUBSCRIPTIONS => {
                        if let Ok(package) =
                            serde_json::from_slice::<SyncSubscriptionsPackage>(&payload)
                        {
                            pulled.subscriptions = package
                                .subscriptions
                                .into_iter()
                                .map(|subscription| SharedSubscription {
                                    url: subscription.channel.url,
                                    name: subscription.channel.name,
                                })
                                .collect();
                            if quiet_until.is_none() {
                                session.set_read_timeout(GRACE)?;
                                quiet_until = Some(Instant::now() + GRACE);
                            }
                        }
                    }
                    sync_opcode::SYNC_HISTORY => {
                        pulled.history = parse_history_urls(&payload);
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(error) => {
                    // A peer that says nothing more within the exchange's window has said
                    // everything it has; a failure to read at all is the exchange's own.
                    if quiet_until.is_some() {
                        break;
                    }
                    return Err(error);
                }
            }
            if quiet_until.is_some_and(|quiet| Instant::now() >= quiet) {
                break;
            }
        }

        let package = SyncSubscriptionsPackage {
            subscriptions: subscriptions
                .iter()
                .map(|subscription| Subscription {
                    channel: SerializedChannel::named(
                        subscription.url.clone(),
                        (!subscription.name.is_empty()).then(|| subscription.name.clone()),
                    ),
                    creation_time: unix_now_i64(),
                })
                .collect(),
            subscription_removals: BTreeMap::new(),
        };
        let json = serde_json::to_string(&package)
            .map_err(|error| Error::Transport(format!("encode subscriptions: {error}")))?;
        session.send_data(sync_opcode::SYNC_SUBSCRIPTIONS, &json)?;
        Ok(pulled)
    }
}

fn parse_history_urls(payload: &[u8]) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Video {
        #[serde(default)]
        url: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Entry {
        #[serde(default)]
        video: Option<Video>,
    }
    serde_json::from_slice::<Vec<Entry>>(payload)
        .map(|list| {
            list.into_iter()
                .filter_map(|entry| entry.video.and_then(|video| video.url))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn private(seed: u8) -> DeviceKey {
        DeviceKey::from_private(&[seed; 32]).unwrap()
    }

    /// A device's own side of one exchange, run on a thread: accept, authorize, answer the
    /// state exchange with `subscriptions` and `history`, then read what the client pushed.
    fn serve_once(
        listener: TcpListener,
        device: DeviceKey,
        code: Option<&'static str>,
        subscriptions: Vec<&'static str>,
        history: Vec<&'static str>,
    ) -> thread::JoinHandle<Vec<String>> {
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let mut session =
                Session::accept_responder(stream, &device, "peer", |_, carried| carried == code)
                    .unwrap();
            session
                .complete_authorization(tilo_test_env::hang_deadline())
                .unwrap();

            // Wait for the state-exchange request. A connection that only paired sends
            // none: it closes, and that is the whole of it.
            loop {
                match session.recv() {
                    Ok(Incoming::Data { sub_opcode, .. })
                        if sub_opcode == sync_opcode::SYNC_STATE_EXCHANGE =>
                    {
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => return Vec::new(),
                }
            }
            let package = SyncSubscriptionsPackage {
                subscriptions: subscriptions
                    .into_iter()
                    .map(|url| Subscription {
                        channel: SerializedChannel::from_url(url),
                        creation_time: 111,
                    })
                    .collect(),
                subscription_removals: BTreeMap::new(),
            };
            session
                .send_data(
                    sync_opcode::SYNC_SUBSCRIPTIONS,
                    &serde_json::to_string(&package).unwrap(),
                )
                .unwrap();
            let entries: Vec<serde_json::Value> = history
                .into_iter()
                .map(|url| serde_json::json!({ "video": { "url": url } }))
                .collect();
            session
                .send_data(
                    sync_opcode::SYNC_HISTORY,
                    &serde_json::to_string(&entries).unwrap(),
                )
                .unwrap();

            loop {
                match session.recv() {
                    Ok(Incoming::Data {
                        sub_opcode,
                        payload,
                    }) if sub_opcode == sync_opcode::SYNC_SUBSCRIPTIONS => {
                        let package: SyncSubscriptionsPackage =
                            serde_json::from_slice(&payload).unwrap();
                        return package
                            .subscriptions
                            .into_iter()
                            .map(|subscription| subscription.channel.url)
                            .collect();
                    }
                    Ok(_) => {}
                    Err(_) => return Vec::new(),
                }
            }
        })
    }

    tilo_test_env::scenario! {
        fn the_grayjay_transport_frames_pairs_streams_and_bounds_payloads;

        phase the_transport_frames_a_message_with_its_length_first;
        {
            {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let address = listener.local_addr().unwrap();
                let serve = thread::spawn(move || {
                    let (mut stream, _) = listener.accept().unwrap();
                    write_frame(&mut stream, b"noise").unwrap();
                    read_frame(&mut stream).unwrap()
                });
                let mut client = TcpStream::connect(address).unwrap();
                assert_eq!(read_frame(&mut client).unwrap(), b"noise");
                write_frame(&mut client, b"back").unwrap();
                assert_eq!(serve.join().unwrap(), b"back");
            }
            {
                // The frame's four length bytes are little-endian, as Grayjay writes them.
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let address = listener.local_addr().unwrap();
                let serve = thread::spawn(move || {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut bytes = [0u8; 9];
                    stream.read_exact(&mut bytes).unwrap();
                    bytes
                });
                let mut client = TcpStream::connect(address).unwrap();
                write_frame(&mut client, b"noise").unwrap();
                assert_eq!(serve.join().unwrap(), [5, 0, 0, 0, b'n', b'o', b'i', b's', b'e']);
            }
        }

        phase an_invite_refuses_a_key_the_device_does_not_hold;
        {
            let peer_key = private(7);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = i32::from(listener.local_addr().unwrap().port());
            let serve = serve_once(listener, peer_key, Some("SECRET42"), vec![], vec![]);

            let invite = serde_json::to_string(&SyncDeviceInfo {
                public_key: private(11).public_base64(),
                addresses: vec!["127.0.0.1".into()],
                port,
                code: Some("SECRET42".into()),
            })
            .unwrap();
            // The invite names a key the device does not hold: the handshake never opens,
            // and no route comes back.
            let client = SyncClient::new(private(9), "Tilo Tube");
            assert!(client.pair(&invite).is_err());
            drop(serve);
        }

        phase an_invite_carries_its_code_and_the_exchange_runs_both_ways;
        {
            let peer_key = private(7);
            let peer_public = peer_key.public_base64();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = i32::from(listener.local_addr().unwrap().port());
            let serve = serve_once(
                listener,
                peer_key,
                Some("SECRET42"),
                vec!["https://youtube.com/@alice", "https://youtube.com/@bob"],
                vec!["https://youtube.com/watch?v=one"],
            );

            let info = SyncDeviceInfo {
                public_key: peer_public.clone(),
                addresses: vec!["127.0.0.1".into()],
                port,
                code: Some("SECRET42".into()),
            };
            let wrapped = format!(
                "grayjay://sync/{}",
                b64_encode(serde_json::to_string(&info).unwrap().as_bytes())
            );
            let client = SyncClient::new(private(9), "Tilo Tube");
            let route = client.pair(&wrapped).unwrap();
            assert_eq!(route.key, peer_public, "the route is the peer's own key");
            assert_eq!(route.port, port);
            assert_eq!(route.addresses, vec!["127.0.0.1".to_string()]);
            assert!(route.name.starts_with("Grayjay "));

            let pushed = serve.join().unwrap();
            assert!(pushed.is_empty(), "adding a device sends no subscriptions yet");

            // The paired device, as the app recorded it, syncs through its route.
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = i32::from(listener.local_addr().unwrap().port());
            let serve = serve_once(
                listener,
                private(7),
                None,
                vec!["https://youtube.com/@carol"],
                vec!["https://youtube.com/watch?v=two"],
            );
            let route = PeerRoute {
                key: private(7).public_base64(),
                name: "Grayjay peer".into(),
                addresses: vec!["127.0.0.1".into()],
                port,
            };
            let pulled = client
                .sync(
                    &route,
                    &[SharedSubscription {
                        url: "https://youtube.com/@mine".into(),
                        name: "Mine".into(),
                    }],
                )
                .unwrap();
            assert_eq!(
                pulled.subscriptions,
                vec![SharedSubscription {
                    url: "https://youtube.com/@carol".into(),
                    name: "https://youtube.com/@carol".into(),
                }]
            );
            assert_eq!(
                pulled.history,
                vec!["https://youtube.com/watch?v=two".to_string()]
            );
            assert_eq!(serve.join().unwrap(), vec!["https://youtube.com/@mine"]);
        }

        phase an_oversized_package_streams_across_packets;
        {
            let subscriptions: Vec<String> = (0..4000)
                .map(|index| format!("https://youtube.com/channel/UC{index:08}xxxxxxxx"))
                .collect();
            let package = SyncSubscriptionsPackage {
                subscriptions: subscriptions
                    .iter()
                    .map(|url| Subscription {
                        channel: SerializedChannel::from_url(url),
                        creation_time: 0,
                    })
                    .collect(),
                subscription_removals: BTreeMap::new(),
            };
            let json = serde_json::to_string(&package).unwrap();
            assert!(
                json.len() > MAXIMUM_PACKET_SIZE,
                "the payload must exceed one packet to be streamed"
            );
            let expected = subscriptions.len();

            let peer_key = private(7);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = i32::from(listener.local_addr().unwrap().port());
            let serve = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                let mut session =
                    Session::accept_responder(stream, &peer_key, "peer", |_, _| true).unwrap();
                session
                    .complete_authorization(tilo_test_env::hang_deadline())
                    .unwrap();
                session
                    .send_data(sync_opcode::SYNC_SUBSCRIPTIONS, &json)
                    .unwrap();
                thread::sleep(Duration::from_millis(100));
            });

            let stream = TcpStream::connect(("127.0.0.1", u16::try_from(port).unwrap())).unwrap();
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let mut session = Session::connect_initiator(
                stream,
                &private(9),
                &private(7).public_base64(),
                None,
                "Tilo Tube",
            )
            .unwrap();
            session
                .complete_authorization(tilo_test_env::hang_deadline())
                .unwrap();

            let got = loop {
                if let Incoming::Data {
                    sub_opcode,
                    payload,
                } = session.recv().unwrap()
                    && sub_opcode == sync_opcode::SYNC_SUBSCRIPTIONS
                {
                    let received: SyncSubscriptionsPackage =
                        serde_json::from_slice(&payload).unwrap();
                    break received.subscriptions.len();
                }
            };
            serve.join().unwrap();
            assert_eq!(got, expected);
        }

        phase invites_and_payloads_stay_bounded;
        {
            {
                let json = serde_json::to_string(&SyncDeviceInfo {
                    public_key: b64_encode(&[3; 32]),
                    addresses: vec!["10.0.0.2".into()],
                    port: 12315,
                    code: Some("Xy".into()),
                })
                .unwrap();
                let parsed = parse_invite(&json).unwrap();
                assert_eq!(parsed.port, 12315);
                let wrapped = format!("grayjay://sync/{}", b64_encode(json.as_bytes()));
                assert_eq!(parse_invite(&wrapped).unwrap(), parsed);
            }
            {
                assert!(parse_invite(&"x".repeat(MAX_INVITE_BYTES + 1)).is_err());
                let too_many_addresses = serde_json::to_string(&SyncDeviceInfo {
                    public_key: b64_encode(&[4; 32]),
                    addresses: vec!["127.0.0.1".into(); MAX_PEER_ADDRESSES + 1],
                    port: 12315,
                    code: Some("ok".into()),
                })
                .unwrap();
                assert!(parse_invite(&too_many_addresses).is_err());
                // A key that is not 32 bytes is no Noise static key.
                let short_key = serde_json::to_string(&SyncDeviceInfo {
                    public_key: b64_encode(&[5; 16]),
                    addresses: vec!["127.0.0.1".into()],
                    port: 12315,
                    code: None,
                })
                .unwrap();
                assert!(parse_invite(&short_key).is_err());
            }
            {
                let compressed = gzip_compress(&vec![0; MAX_SYNC_PAYLOAD_BYTES + 1]).unwrap();
                let error = gzip_decompress(&compressed).unwrap_err();
                assert!(error.to_string().contains("resource bound"));
            }
        }
    }
}
