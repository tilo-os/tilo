//! RTP packets on a voice server's UDP socket, and their transport
//! encryption in Discord's `*_rtpsize` AEAD modes.
//!
//! In those modes the RTP header, its CSRCs and the 4-byte preamble of a
//! header extension stay in the clear and are the AEAD's associated data;
//! the extension's body and the Opus payload are encrypted, the 16-byte tag
//! follows, and a 32-bit big-endian counter is appended in the clear. The
//! counter is the nonce's first four bytes, the rest of the nonce is zero.

pub use tilo_seal::discord_voice::{AES_GCM, TransportCipher as Cipher, XCHACHA, choose_mode};

/// RTP payload type Discord uses for Opus.
pub const OPUS_PAYLOAD_TYPE: u8 = 0x78;
/// Samples per 20 ms Opus frame at 48 kHz: the RTP timestamp's step.
pub const FRAME_SAMPLES: u32 = 960;
const HEADER: usize = 12;
const NONCE_SUFFIX: usize = 4;

/// What the worker sends: one Opus frame (already end-to-end encrypted
/// when DAVE is on) per packet, numbered on from a random start.
pub struct Sender {
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    counter: u32,
}

impl Sender {
    #[must_use]
    pub fn new(ssrc: u32, start: [u8; 10]) -> Self {
        let [s0, s1, t0, t1, t2, t3, c0, c1, c2, c3] = start;
        Self {
            ssrc,
            sequence: u16::from_be_bytes([s0, s1]),
            timestamp: u32::from_be_bytes([t0, t1, t2, t3]),
            counter: u32::from_be_bytes([c0, c1, c2, c3]),
        }
    }

    /// A frame that is not sent: its place in the timeline is still spent, so
    /// the receiver sees the sequence and the timestamp advance without a
    /// packet and conceals the 20 ms it never got, instead of decoding the
    /// next frame as though it followed the last one.
    pub fn skip(&mut self) {
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(FRAME_SAMPLES);
    }

    /// The next packet carrying `frame`, sealed with `cipher`.
    pub fn packet(&mut self, cipher: &Cipher, frame: &[u8]) -> Option<Vec<u8>> {
        let mut packet = Vec::with_capacity(HEADER.saturating_add(frame.len()).saturating_add(32));
        packet.extend_from_slice(&[0x80, OPUS_PAYLOAD_TYPE]);
        packet.extend_from_slice(&self.sequence.to_be_bytes());
        packet.extend_from_slice(&self.timestamp.to_be_bytes());
        packet.extend_from_slice(&self.ssrc.to_be_bytes());
        let sealed = cipher.seal(self.counter, &packet, frame).ok()?;
        packet.extend_from_slice(&sealed);
        packet.extend_from_slice(&self.counter.to_be_bytes());
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(FRAME_SAMPLES);
        self.counter = self.counter.wrapping_add(1);
        Some(packet)
    }
}

/// One RTP packet received, opened.
#[derive(Debug, PartialEq, Eq)]
pub struct Received {
    pub ssrc: u32,
    pub sequence: u16,
    /// The media frame: Opus, or a DAVE frame wrapping it.
    pub frame: Vec<u8>,
}

/// Open an RTP packet from the voice server. `None` for RTCP, for anything
/// that is not RTP version 2 carrying Opus, and for what does not decrypt.
#[must_use]
pub fn open(cipher: &Cipher, packet: &[u8]) -> Option<Received> {
    let (&first, &kind) = (packet.first()?, packet.get(1)?);
    if first >> 6 != 2 || kind & 0x7f != OPUS_PAYLOAD_TYPE {
        return None;
    }
    let csrcs = usize::from(first & 0x0f);
    let has_extension = first & 0x10 != 0;
    let mut header = HEADER.checked_add(csrcs.checked_mul(4)?)?;
    let mut extension_words = 0_usize;
    if has_extension {
        let preamble = packet.get(header..header.checked_add(4)?)?;
        extension_words = usize::from(u16::from_be_bytes([*preamble.get(2)?, *preamble.get(3)?]));
        header = header.checked_add(4)?;
    }
    let body_end = packet.len().checked_sub(NONCE_SUFFIX)?;
    let aad = packet.get(..header)?;
    let sealed = packet.get(header..body_end)?;
    let counter: [u8; 4] = packet.get(body_end..)?.try_into().ok()?;
    let mut plain = cipher.open(u32::from_be_bytes(counter), aad, sealed).ok()?;
    let skip = extension_words.checked_mul(4)?;
    if skip > plain.len() {
        return None;
    }
    plain.drain(..skip);
    Some(Received {
        ssrc: u32::from_be_bytes(packet.get(8..12)?.try_into().ok()?),
        sequence: u16::from_be_bytes(packet.get(2..4)?.try_into().ok()?),
        frame: plain,
    })
}
