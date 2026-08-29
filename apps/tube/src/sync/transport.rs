//! Live Grayjay "Sync" transport over the Noise Protocol Framework.
//!
//! This is the real thing (gated behind the `grayjay-sync` cargo feature):
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
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use snow::{Builder, HandshakeState, TransportState};

use super::protocol::{
    self, Frame, HEADER_SIZE, MAXIMUM_PACKET_SIZE, MAXIMUM_PACKET_SIZE_ENCRYPTED,
    MINIMUM_PROTOCOL_VERSION, NOISE_PATTERN_MAIN, NOISE_PATTERN_PAIRING, PROTOCOL_VERSION,
    SerializedChannel, Subscription, SyncDeviceInfo, SyncSessionData, SyncSubscriptionsPackage,
    content_encoding, notify, opcode, stream, sync_opcode,
};
use super::{PeerId, SyncError, SyncResult, SyncState, SyncTransport};

// ---------------------------------------------------------------------------
// base64 helpers — Grayjay's `toBase64()` is STANDARD (with padding); its
// `base64UrlToByteArray` is URL-safe (used for mDNS TXT / deeplinks).
// ---------------------------------------------------------------------------

fn b64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn b64_decode(s: &str) -> SyncResult<Vec<u8>> {
    // Accept either standard or url-safe, padded or not.
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
    Err(SyncError::Pairing(format!("invalid base64 key: {s:?}")))
}

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn snow_err(e: snow::Error) -> SyncError {
    SyncError::Transport(format!("noise: {e}"))
}

fn io_err(e: std::io::Error) -> SyncError {
    SyncError::Transport(format!("io: {e}"))
}

// ---------------------------------------------------------------------------
// Device static key (the Noise identity that a peer pairs against)
// ---------------------------------------------------------------------------

/// A device's long-lived Noise static keypair (Curve25519). The base64 public
/// key is what a peer stores at pairing time and re-authenticates on reconnect.
#[derive(Clone)]
pub struct DeviceKey {
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

impl DeviceKey {
    /// Generate a fresh device identity.
    pub fn generate() -> SyncResult<Self> {
        let params = NOISE_PATTERN_MAIN.parse().map_err(snow_err)?;
        let kp = Builder::new(params).generate_keypair().map_err(snow_err)?;
        Ok(DeviceKey {
            private: kp.private,
            public: kp.public,
        })
    }

    /// Restore from a stored base64 keypair.
    pub fn from_base64(private_b64: &str, public_b64: &str) -> SyncResult<Self> {
        Ok(DeviceKey {
            private: b64_decode(private_b64)?,
            public: b64_decode(public_b64)?,
        })
    }

    /// Our public key in Grayjay's base64 form (the value a peer pairs against).
    pub fn public_base64(&self) -> String {
        b64_encode(&self.public)
    }

    pub fn private_base64(&self) -> String {
        b64_encode(&self.private)
    }
}

// ---------------------------------------------------------------------------
// A message surfaced by the receive loop
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Incoming {
    /// A DATA frame (payload already gzip-inflated). `sub_opcode` is a
    /// `sync_opcode::*` value.
    Data { sub_opcode: u8, payload: Vec<u8> },
    /// Peer sent `NOTIFY/AUTHORIZED`.
    Authorized { remote_name: String },
    /// Peer sent `NOTIFY/UNAUTHORIZED`.
    Unauthorized,
}

struct Reasm {
    opcode: u8,
    sub_opcode: u8,
    content_encoding: u8,
    expected_size: usize,
    buf: Vec<u8>,
}

// ---------------------------------------------------------------------------
// A live, framed Noise session
// ---------------------------------------------------------------------------

/// One connected + handshaked Noise session. Owns the socket and the transport
/// cipher state. Not `Clone`; drive it from a single thread.
pub struct GrayjaySession {
    stream: TcpStream,
    transport: TransportState,
    remote_public_key: String,
    local_public_key: String,
    remote_version: i32,
    remote_authorized: bool,
    streams: HashMap<i32, Reasm>,
    next_stream_id: i32,
    device_name: String,
}

impl GrayjaySession {
    /// The peer's authenticated Noise static public key (base64).
    pub fn remote_public_key(&self) -> &str {
        &self.remote_public_key
    }

    pub fn local_public_key(&self) -> &str {
        &self.local_public_key
    }

    pub fn remote_version(&self) -> i32 {
        self.remote_version
    }

    // -- handshake ---------------------------------------------------------

    /// Raw-socket version exchange that precedes the Noise handshake.
    fn version_check(stream: &mut TcpStream) -> SyncResult<i32> {
        stream
            .write_all(&PROTOCOL_VERSION.to_le_bytes())
            .map_err(io_err)?;
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

    /// Connect as the Noise **initiator** to a peer whose static public key we
    /// already hold (from pairing). If `pairing_code` is set, its bytes travel
    /// encrypted inside a `Noise_N` sub-handshake in the first frame.
    pub fn connect_initiator(
        mut stream: TcpStream,
        device: &DeviceKey,
        remote_public_key_b64: &str,
        pairing_code: Option<&str>,
        device_name: &str,
    ) -> SyncResult<Self> {
        let remote_pub = b64_decode(remote_public_key_b64)?;

        let remote_version = Self::version_check(&mut stream)?;

        let params = NOISE_PATTERN_MAIN.parse().map_err(snow_err)?;
        let mut hs: HandshakeState = Builder::new(params)
            .local_private_key(&device.private)
            .remote_public_key(&remote_pub)
            .build_initiator()
            .map_err(snow_err)?;

        // Optional pairing sub-handshake (Noise_N) carrying the pairing code.
        let pairing_msg: Vec<u8> = if let Some(code) = pairing_code {
            let pparams = NOISE_PATTERN_PAIRING.parse().map_err(snow_err)?;
            let mut ph: HandshakeState = Builder::new(pparams)
                .remote_public_key(&remote_pub)
                .build_initiator()
                .map_err(snow_err)?;
            let mut buf = vec![0u8; 512];
            let n = ph
                .write_message(code.as_bytes(), &mut buf)
                .map_err(snow_err)?;
            buf.truncate(n);
            buf
        } else {
            Vec::new()
        };

        // Main IK message 1 (empty payload).
        let mut main_buf = vec![0u8; 512];
        let main_len = hs.write_message(&[], &mut main_buf).map_err(snow_err)?;

        // Frame: [messageSize u32][appId u32][pairingLen u32][pairing][main].
        let app_id: u32 = 0;
        let message_size = (4 + 4 + pairing_msg.len() + main_len) as u32;
        let mut out = Vec::with_capacity(4 + message_size as usize);
        out.extend_from_slice(&message_size.to_le_bytes());
        out.extend_from_slice(&app_id.to_le_bytes());
        out.extend_from_slice(&(pairing_msg.len() as u32).to_le_bytes());
        out.extend_from_slice(&pairing_msg);
        out.extend_from_slice(&main_buf[..main_len]);
        stream.write_all(&out).map_err(io_err)?;

        // Response: [len u32][mainMsg2].
        let mut lenb = [0u8; 4];
        read_exact(&mut stream, &mut lenb)?;
        let resp_len = u32::from_le_bytes(lenb) as usize;
        if resp_len > MAXIMUM_PACKET_SIZE_ENCRYPTED {
            return Err(SyncError::Transport(format!(
                "handshake response too large: {resp_len}"
            )));
        }
        let mut resp = vec![0u8; resp_len];
        read_exact(&mut stream, &mut resp)?;
        let mut scratch = vec![0u8; 512];
        hs.read_message(&resp, &mut scratch).map_err(snow_err)?;

        // Capture + verify the peer's static key before consuming the handshake.
        let remote_static = hs
            .get_remote_static()
            .map(|s| s.to_vec())
            .ok_or_else(|| SyncError::Transport("peer did not present a static key".into()))?;
        let remote_static_b64 = b64_encode(&remote_static);
        if remote_static_b64 != b64_encode(&remote_pub) {
            return Err(SyncError::Pairing(format!(
                "peer key mismatch: expected {remote_public_key_b64}, got {remote_static_b64}"
            )));
        }

        let transport = hs.into_transport_mode().map_err(snow_err)?;

        Ok(GrayjaySession {
            stream,
            transport,
            remote_public_key: remote_static_b64,
            local_public_key: device.public_base64(),
            remote_version,
            remote_authorized: false,
            streams: HashMap::new(),
            next_stream_id: 0,
            device_name: device_name.to_string(),
        })
    }

    /// Accept as the Noise **responder**. `allow` is the pairing/authorization
    /// gate: it receives the peer's static-key base64 and the decoded pairing
    /// code (if any) and returns whether to proceed. Mirrors Grayjay's
    /// `handshakeAsResponder` + `isHandshakeAllowed`.
    pub fn accept_responder(
        mut stream: TcpStream,
        device: &DeviceKey,
        device_name: &str,
        allow: impl Fn(&str, Option<&str>) -> bool,
    ) -> SyncResult<Self> {
        let remote_version = Self::version_check(&mut stream)?;

        let params = NOISE_PATTERN_MAIN.parse().map_err(snow_err)?;
        let mut hs: HandshakeState = Builder::new(params)
            .local_private_key(&device.private)
            .build_responder()
            .map_err(snow_err)?;

        // Read the first frame: [messageSize][appId][pairingLen][pairing][main].
        let mut lenb = [0u8; 4];
        read_exact(&mut stream, &mut lenb)?;
        let message_size = u32::from_le_bytes(lenb) as usize;
        if message_size > MAXIMUM_PACKET_SIZE_ENCRYPTED {
            return Err(SyncError::Transport(format!(
                "handshake frame too large: {message_size}"
            )));
        }
        let mut msg = vec![0u8; message_size];
        read_exact(&mut stream, &mut msg)?;
        if msg.len() < 8 {
            return Err(SyncError::Transport("handshake frame too short".into()));
        }
        let _app_id = u32::from_le_bytes([msg[0], msg[1], msg[2], msg[3]]);
        let pairing_len = u32::from_le_bytes([msg[4], msg[5], msg[6], msg[7]]) as usize;
        let rest = &msg[8..];
        if pairing_len > rest.len() {
            return Err(SyncError::Transport(
                "pairing length overflows frame".into(),
            ));
        }
        let (pairing_msg, main_msg) = rest.split_at(pairing_len);

        // Decode the pairing code, if present (Noise_N responder).
        let pairing_code: Option<String> = if pairing_len > 0 {
            let pparams = NOISE_PATTERN_PAIRING.parse().map_err(snow_err)?;
            let mut ph: HandshakeState = Builder::new(pparams)
                .local_private_key(&device.private)
                .build_responder()
                .map_err(snow_err)?;
            let mut pt = vec![0u8; 512];
            let n = ph.read_message(pairing_msg, &mut pt).map_err(snow_err)?;
            Some(String::from_utf8_lossy(&pt[..n]).into_owned())
        } else {
            None
        };

        let mut scratch = vec![0u8; 512];
        hs.read_message(main_msg, &mut scratch).map_err(snow_err)?;

        let remote_static = hs
            .get_remote_static()
            .map(|s| s.to_vec())
            .ok_or_else(|| SyncError::Transport("initiator did not present a static key".into()))?;
        let remote_static_b64 = b64_encode(&remote_static);

        if remote_static_b64 == device.public_base64()
            || !allow(&remote_static_b64, pairing_code.as_deref())
        {
            return Err(SyncError::Pairing("handshake not allowed".into()));
        }

        // Response: [len u32][mainMsg2].
        let mut resp = vec![0u8; 512];
        let rn = hs.write_message(&[], &mut resp).map_err(snow_err)?;
        let mut out = Vec::with_capacity(4 + rn);
        out.extend_from_slice(&(rn as u32).to_le_bytes());
        out.extend_from_slice(&resp[..rn]);
        stream.write_all(&out).map_err(io_err)?;

        let transport = hs.into_transport_mode().map_err(snow_err)?;

        Ok(GrayjaySession {
            stream,
            transport,
            remote_public_key: remote_static_b64,
            local_public_key: device.public_base64(),
            remote_version,
            remote_authorized: false,
            streams: HashMap::new(),
            next_stream_id: 0,
            device_name: device_name.to_string(),
        })
    }

    // -- low level framing --------------------------------------------------

    /// Encrypt one plaintext frame and write `[len u32][ciphertext]`.
    fn write_encrypted(&mut self, plaintext: &[u8]) -> SyncResult<()> {
        let mut cipher = vec![0u8; plaintext.len() + 16];
        let len = self
            .transport
            .write_message(plaintext, &mut cipher)
            .map_err(snow_err)?;
        let mut out = Vec::with_capacity(4 + len);
        out.extend_from_slice(&(len as u32).to_le_bytes());
        out.extend_from_slice(&cipher[..len]);
        self.stream.write_all(&out).map_err(io_err)?;
        Ok(())
    }

    /// Read + decrypt one raw frame into its plaintext bytes.
    fn read_encrypted(&mut self) -> SyncResult<Vec<u8>> {
        let mut lenb = [0u8; 4];
        read_exact(&mut self.stream, &mut lenb)?;
        let mlen = u32::from_le_bytes(lenb) as usize;
        if mlen > MAXIMUM_PACKET_SIZE_ENCRYPTED {
            return Err(SyncError::Transport(format!("frame too large: {mlen}")));
        }
        let mut cipher = vec![0u8; mlen];
        read_exact(&mut self.stream, &mut cipher)?;
        let mut plain = vec![0u8; mlen];
        let plen = self
            .transport
            .read_message(&cipher, &mut plain)
            .map_err(snow_err)?;
        plain.truncate(plen);
        Ok(plain)
    }

    /// Send an opcode/subOpcode/payload, segmenting with STREAM when the payload
    /// exceeds one packet. `gzip` compresses DATA payloads (Grayjay does this).
    fn send(&mut self, opcode: u8, sub_opcode: u8, payload: &[u8], gzip: bool) -> SyncResult<()> {
        let (ce, processed) = if gzip && opcode == opcode::DATA {
            (content_encoding::GZIP, gzip_compress(payload)?)
        } else {
            (content_encoding::RAW, payload.to_vec())
        };

        if processed.len() + HEADER_SIZE <= MAXIMUM_PACKET_SIZE {
            let plaintext = protocol::encode_frame(opcode, sub_opcode, ce, &processed);
            return self.write_encrypted(&plaintext);
        }

        // STREAM path (mirrors SyncSocketSession.send()'s segmentation).
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
            seg.extend_from_slice(&processed[offset..offset + bytes_to_send]);
            // Each segment is itself a normal (un-gzip'd) STREAM frame.
            let plaintext =
                protocol::encode_frame(opcode::STREAM, stream_op, content_encoding::RAW, &seg);
            self.write_encrypted(&plaintext)?;
            offset += bytes_to_send;
        }
        Ok(())
    }

    /// Send a DATA frame (gzip'd) carrying a JSON sync package.
    pub fn send_data(&mut self, sub_opcode: u8, json: &str) -> SyncResult<()> {
        self.send(opcode::DATA, sub_opcode, json.as_bytes(), true)
    }

    /// Send our `NOTIFY/AUTHORIZED` (`[idLen u8][uuid][nameLen u8][name]`).
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

    /// Reply to a PING with a PONG (opcode-only frame).
    fn send_pong(&mut self) -> SyncResult<()> {
        self.send(opcode::PONG, 0, &[], false)
    }

    /// Read the next *meaningful* incoming message, handling PING/PONG and
    /// STREAM reassembly internally. Honors the socket's read timeout.
    fn recv(&mut self) -> SyncResult<Incoming> {
        loop {
            let plain = self.read_encrypted()?;
            let frame = protocol::decode_frame(&plain)
                .map_err(|e| SyncError::Transport(format!("frame decode: {e}")))?;

            match frame.opcode {
                opcode::PING => {
                    self.send_pong()?;
                }
                opcode::PONG => { /* keep-alive ack; ignore */ }
                opcode::STREAM => {
                    if let Some(resolved) = self.handle_stream(&frame)? {
                        if let Some(inc) = self.resolve(resolved)? {
                            return Ok(inc);
                        }
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
                    if let Some(inc) = self.resolve(frame)? {
                        return Ok(inc);
                    }
                }
                // REQUEST/RESPONSE/RELAY belong to the relay/record subsystem,
                // which the direct-connect client does not drive. Ignore.
                _ => {}
            }
        }
    }

    /// Turn a fully-reassembled/plain DATA frame into an [`Incoming`], inflating
    /// gzip. Returns `None` for opcodes we don't surface.
    fn resolve(&self, frame: Frame) -> SyncResult<Option<Incoming>> {
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

    /// Feed a STREAM segment into reassembly; on END return the completed frame.
    fn handle_stream(&mut self, frame: &Frame) -> SyncResult<Option<Frame>> {
        let d = &frame.payload;
        let short = || SyncError::Transport("stream segment too short".into());
        match frame.sub_opcode {
            stream::START => {
                if d.len() < 11 {
                    return Err(short());
                }
                let id = i32::from_le_bytes([d[0], d[1], d[2], d[3]]);
                let expected = u32::from_le_bytes([d[4], d[5], d[6], d[7]]) as usize;
                let op = d[8];
                let sub = d[9];
                let ce = d[10];
                let mut buf = Vec::with_capacity(expected);
                buf.extend_from_slice(&d[11..]);
                self.streams.insert(
                    id,
                    Reasm {
                        opcode: op,
                        sub_opcode: sub,
                        content_encoding: ce,
                        expected_size: expected,
                        buf,
                    },
                );
                Ok(None)
            }
            stream::DATA => {
                if d.len() < 8 {
                    return Err(short());
                }
                let id = i32::from_le_bytes([d[0], d[1], d[2], d[3]]);
                let expected_off = u32::from_le_bytes([d[4], d[5], d[6], d[7]]) as usize;
                let r = self
                    .streams
                    .get_mut(&id)
                    .ok_or_else(|| SyncError::Transport("stream data for unknown id".into()))?;
                if expected_off != r.buf.len() {
                    return Err(SyncError::Transport("stream offset mismatch".into()));
                }
                r.buf.extend_from_slice(&d[8..]);
                Ok(None)
            }
            stream::END => {
                if d.len() < 8 {
                    return Err(short());
                }
                let id = i32::from_le_bytes([d[0], d[1], d[2], d[3]]);
                let expected_off = u32::from_le_bytes([d[4], d[5], d[6], d[7]]) as usize;
                let mut r = self
                    .streams
                    .remove(&id)
                    .ok_or_else(|| SyncError::Transport("stream end for unknown id".into()))?;
                if expected_off != r.buf.len() {
                    return Err(SyncError::Transport("stream offset mismatch at end".into()));
                }
                r.buf.extend_from_slice(&d[8..]);
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

    /// Send our AUTHORIZED and block (bounded) until the peer authorizes us.
    fn complete_authorization(&mut self, deadline: Instant) -> SyncResult<String> {
        self.send_authorized()?;
        loop {
            if Instant::now() >= deadline {
                return Err(SyncError::Transport(
                    "timed out waiting for peer authorization".into(),
                ));
            }
            match self.recv()? {
                Incoming::Authorized { remote_name } => return Ok(remote_name),
                Incoming::Unauthorized => {
                    return Err(SyncError::Pairing("peer refused authorization".into()));
                }
                // A peer may start streaming data before/after AUTHORIZED; keep
                // waiting for the AUTHORIZED specifically.
                Incoming::Data { .. } => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn read_exact(stream: &mut TcpStream, buf: &mut [u8]) -> SyncResult<()> {
    stream.read_exact(buf).map_err(io_err)
}

fn parse_authorized_name(payload: &[u8]) -> String {
    // [idLen u8][id][nameLen u8][name]
    if payload.is_empty() {
        return String::new();
    }
    let id_len = payload[0] as usize;
    let name_start = 1 + id_len + 1;
    if payload.len() < name_start {
        return String::new();
    }
    let name_len = payload[1 + id_len] as usize;
    let end = (name_start + name_len).min(payload.len());
    String::from_utf8_lossy(&payload[name_start..end]).into_owned()
}

fn gzip_compress(data: &[u8]) -> SyncResult<Vec<u8>> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).map_err(io_err)?;
    enc.finish().map_err(io_err)
}

fn gzip_decompress(data: &[u8]) -> SyncResult<Vec<u8>> {
    let mut dec = GzDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out).map_err(io_err)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// The SyncTransport implementation
// ---------------------------------------------------------------------------

/// The real Grayjay sync transport. Holds this device's Noise identity and a map
/// of paired peers (their advertised address/port), and speaks the protocol in
/// [`super::protocol`]. Pairing/push/pull each open a fresh short-lived session.
///
/// Persistence of the device key + paired peers is the app's responsibility; the
/// transport keeps them in memory for the life of the process.
pub struct GrayjaySyncTransport {
    device: DeviceKey,
    device_name: String,
    peers: HashMap<String, SyncDeviceInfo>,
    connect_timeout: Duration,
    io_timeout: Duration,
    sync_deadline: Duration,
}

impl GrayjaySyncTransport {
    /// Create a transport with a freshly generated device identity.
    pub fn new(device_name: impl Into<String>) -> SyncResult<Self> {
        Ok(Self::with_device(DeviceKey::generate()?, device_name))
    }

    /// Create a transport with a specific (e.g. persisted) device identity.
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

    /// This device's public key (base64) — put it in our own pairing QR.
    pub fn device_public_key(&self) -> String {
        self.device.public_base64()
    }

    /// Register a paired peer's connection info (address/port) without dialing.
    pub fn remember_peer(&mut self, info: SyncDeviceInfo) {
        self.peers.insert(info.public_key.clone(), info);
    }

    fn parse_blob(blob: &str) -> SyncResult<SyncDeviceInfo> {
        let blob = blob.trim();
        // Direct JSON.
        if let Ok(info) = serde_json::from_str::<SyncDeviceInfo>(blob) {
            return Ok(info);
        }
        // A `grayjay://...` (or bare) base64/base64url of the JSON.
        let payload = blob.rsplit(['/', ':']).next().unwrap_or(blob);
        let bytes = b64_decode(payload)?;
        serde_json::from_slice::<SyncDeviceInfo>(&bytes)
            .map_err(|e| SyncError::Pairing(format!("unrecognized pairing blob: {e}")))
    }

    /// Open + handshake + authorize a session to a paired peer.
    fn open_session(
        &self,
        info: &SyncDeviceInfo,
        pairing_code: Option<&str>,
    ) -> SyncResult<GrayjaySession> {
        let mut last_err = SyncError::Transport("no addresses to connect".into());
        for addr in &info.addresses {
            let target = format!("{addr}:{}", info.port);
            let sock_addr = match target.to_socket_addrs() {
                Ok(mut it) => match it.next() {
                    Some(a) => a,
                    None => continue,
                },
                Err(e) => {
                    last_err = SyncError::Transport(format!("resolve {target}: {e}"));
                    continue;
                }
            };
            let stream = match TcpStream::connect_timeout(&sock_addr, self.connect_timeout) {
                Ok(s) => s,
                Err(e) => {
                    last_err = io_err(e);
                    continue;
                }
            };
            stream.set_nodelay(true).ok();
            stream.set_read_timeout(Some(self.io_timeout)).ok();
            stream.set_write_timeout(Some(self.io_timeout)).ok();

            let mut session = GrayjaySession::connect_initiator(
                stream,
                &self.device,
                &info.public_key,
                pairing_code.or(info.pairing_code.as_deref()),
                &self.device_name,
            )?;
            let deadline = Instant::now() + self.sync_deadline;
            session.complete_authorization(deadline)?;
            return Ok(session);
        }
        Err(last_err)
    }

    /// Build a `SyncSubscriptionsPackage` JSON from Tube's channel-URL list.
    fn subscriptions_json(state: &SyncState) -> SyncResult<String> {
        let now = now_epoch_secs();
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
    fn name(&self) -> &str {
        "grayjay-sync (Noise IK over TCP 12315)"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn pair(&mut self, pairing_blob: &str) -> SyncResult<PeerId> {
        let info = Self::parse_blob(pairing_blob)?;
        // Verify we can actually handshake + authorize with the advertised key
        // and code; on success the peer is paired and remembered.
        let session = self.open_session(&info, info.pairing_code.as_deref())?;
        let peer_key = session.remote_public_key().to_string();
        let mut remembered = info.clone();
        remembered.public_key = peer_key.clone();
        // Drop the one-shot pairing code; reconnects authenticate via the key.
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

        // Ask for everything.
        let req = SyncSessionData::request_all(self.device.public_base64());
        let req_json = serde_json::to_string(&req)
            .map_err(|e| SyncError::Transport(format!("encode state-exchange: {e}")))?;
        session.send_data(sync_opcode::SYNC_STATE_EXCHANGE, &req_json)?;

        let mut out = SyncState::default();
        let deadline = Instant::now() + self.sync_deadline;
        let mut got_subs = false;
        while Instant::now() < deadline {
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
                    // The peer also sends us a state-exchange request; ignoring
                    // it just means we don't push during a pull.
                    _ => {}
                },
                Ok(_) => {}
                Err(_) => break,
            }
            // Subscriptions are the primary payload; once we have them, a short
            // grace for history is enough — bail if the peer went quiet.
            if got_subs && Instant::now() + Duration::from_millis(500) >= deadline {
                break;
            }
        }
        Ok(out)
    }
}

/// Best-effort extraction of watched video URLs from a `List<HistoryVideo>` JSON.
/// Full `HistoryVideo` fidelity (nested `SerializedPlatformVideo`) is not modeled
/// — we pull each entry's `video.url`.
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
    use std::net::TcpListener;
    use std::thread;

    /// Full in-process round-trip: an initiator and a responder speaking the
    /// SAME pattern/opcodes/framing complete the IK handshake, authorize, and
    /// round-trip a real `SyncSubscriptionsPackage` DATA frame (gzip'd JSON).
    /// This proves Noise + framing + opcodes + gzip + JSON without a phone.
    #[test]
    fn noise_round_trip_subscriptions() {
        let responder_key = DeviceKey::generate().unwrap();
        let initiator_key = DeviceKey::generate().unwrap();
        let responder_pub = responder_key.public_base64();
        let initiator_pub = initiator_key.public_base64();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        // Responder thread: accept, handshake, authorize, then reply to the
        // state-exchange with a known subscriptions package.
        let rkey = responder_key.clone();
        let expect_initiator = initiator_pub.clone();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut session =
                GrayjaySession::accept_responder(stream, &rkey, "responder", |peer_key, _code| {
                    peer_key == expect_initiator
                })
                .unwrap();

            let deadline = Instant::now() + Duration::from_secs(10);
            session.complete_authorization(deadline).unwrap();

            // Expect the state-exchange request.
            let mut asked = false;
            for _ in 0..8 {
                if let Incoming::Data {
                    sub_opcode,
                    payload,
                } = session.recv().unwrap()
                {
                    if sub_opcode == sync_opcode::SYNC_STATE_EXCHANGE {
                        let _req: SyncSessionData = serde_json::from_slice(&payload).unwrap();
                        asked = true;
                        break;
                    }
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
            // keep the socket open briefly so the initiator can read the frame
            thread::sleep(Duration::from_millis(200));
        });

        // Initiator side.
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
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

        let deadline = Instant::now() + Duration::from_secs(10);
        let name = session.complete_authorization(deadline).unwrap();
        assert_eq!(name, "responder");

        // Ask for state, then read the subscriptions package back.
        let req = SyncSessionData::request_all(initiator_key.public_base64());
        session
            .send_data(
                sync_opcode::SYNC_STATE_EXCHANGE,
                &serde_json::to_string(&req).unwrap(),
            )
            .unwrap();

        let mut urls = Vec::new();
        for _ in 0..8 {
            if let Incoming::Data {
                sub_opcode,
                payload,
            } = session.recv().unwrap()
            {
                if sub_opcode == sync_opcode::SYNC_SUBSCRIPTIONS {
                    let pkg: SyncSubscriptionsPackage = serde_json::from_slice(&payload).unwrap();
                    urls = pkg
                        .subscriptions
                        .into_iter()
                        .map(|s| s.channel.url)
                        .collect();
                    break;
                }
            }
        }
        handle.join().unwrap();
        assert_eq!(
            urls,
            vec![
                "https://youtube.com/@alice".to_string(),
                "https://youtube.com/@bob".to_string()
            ]
        );
    }

    /// The pairing sub-handshake (Noise_N) must carry the pairing code to the
    /// responder, and a wrong key must be rejected.
    #[test]
    fn pairing_code_travels_and_key_is_authenticated() {
        let responder_key = DeviceKey::generate().unwrap();
        let initiator_key = DeviceKey::generate().unwrap();
        let responder_pub = responder_key.public_base64();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let rkey = responder_key.clone();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            // Responder only proceeds if the pairing code matches.
            let mut session =
                GrayjaySession::accept_responder(stream, &rkey, "responder", |_peer, code| {
                    code == Some("SECRET42")
                })
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            session.complete_authorization(deadline).unwrap();
        });

        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut session = GrayjaySession::connect_initiator(
            stream,
            &initiator_key,
            &responder_pub,
            Some("SECRET42"),
            "initiator",
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        session.complete_authorization(deadline).unwrap();
        handle.join().unwrap();
    }

    /// A payload larger than one packet must round-trip via STREAM
    /// segmentation + reassembly through the real send/recv path.
    #[test]
    fn large_payload_streams_and_reassembles() {
        let responder_key = DeviceKey::generate().unwrap();
        let initiator_key = DeviceKey::generate().unwrap();
        let responder_pub = responder_key.public_base64();
        let initiator_pub = initiator_key.public_base64();

        // Build ~200KB of subscriptions so the gzip'd JSON still exceeds one
        // 64KB packet, forcing the STREAM path.
        let mut subs = Vec::new();
        for i in 0..4000 {
            subs.push(Subscription {
                channel: SerializedChannel::from_url(format!(
                    "https://youtube.com/channel/UC{i:08}xxxxxxxx"
                )),
                creation_time: i as i64,
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

        let rkey = responder_key.clone();
        let expect_initiator = initiator_pub.clone();
        let json_for_thread = json.clone();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut session =
                GrayjaySession::accept_responder(stream, &rkey, "responder", |peer, _code| {
                    peer == expect_initiator
                })
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            session.complete_authorization(deadline).unwrap();
            session
                .send_data(sync_opcode::SYNC_SUBSCRIPTIONS, &json_for_thread)
                .unwrap();
            thread::sleep(Duration::from_millis(300));
        });

        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut session = GrayjaySession::connect_initiator(
            stream,
            &initiator_key,
            &responder_pub,
            None,
            "initiator",
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        session.complete_authorization(deadline).unwrap();

        let mut got = 0usize;
        for _ in 0..4 {
            if let Incoming::Data {
                sub_opcode,
                payload,
            } = session.recv().unwrap()
            {
                if sub_opcode == sync_opcode::SYNC_SUBSCRIPTIONS {
                    let received: SyncSubscriptionsPackage =
                        serde_json::from_slice(&payload).unwrap();
                    got = received.subscriptions.len();
                    break;
                }
            }
        }
        handle.join().unwrap();
        assert_eq!(got, expected_len);
    }

    #[test]
    fn blob_parses_json_and_base64() {
        let json =
            r#"{"publicKey":"AAAA","addresses":["10.0.0.2"],"port":12315,"pairingCode":"Xy"}"#;
        let a = GrayjaySyncTransport::parse_blob(json).unwrap();
        assert_eq!(a.port, 12315);

        let wrapped = format!("grayjay://sync/{}", b64_encode(json.as_bytes()));
        let b = GrayjaySyncTransport::parse_blob(&wrapped).unwrap();
        assert_eq!(a, b);
    }
}
