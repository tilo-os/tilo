//! RTP on the fake voice server's UDP socket, in Discord's `*_rtpsize`
//! AEAD modes: the header (and an extension's preamble) is associated
//! data, the payload is sealed, and a 32-bit counter appended in the clear
//! is the nonce's first four bytes.

pub use tilo_seal::discord_voice::{AES_GCM, TransportCipher as Cipher, XCHACHA};

/// One sender's RTP stream.
pub struct Stream {
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    counter: u32,
}

impl Stream {
    pub fn new(ssrc: u32, start: [u8; 10]) -> Self {
        let [s0, s1, t0, t1, t2, t3, c0, c1, c2, c3] = start;
        Self {
            ssrc,
            sequence: u16::from_be_bytes([s0, s1]),
            timestamp: u32::from_be_bytes([t0, t1, t2, t3]),
            counter: u32::from_be_bytes([c0, c1, c2, c3]),
        }
    }

    pub const fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// The next packet carrying `frame`. It carries a one-word header
    /// extension, as Discord's clients send, so a receiver must strip it.
    pub fn packet(&mut self, cipher: &Cipher, frame: &[u8]) -> Option<Vec<u8>> {
        let mut packet = vec![0x90, 0x78];
        packet.extend_from_slice(&self.sequence.to_be_bytes());
        packet.extend_from_slice(&self.timestamp.to_be_bytes());
        packet.extend_from_slice(&self.ssrc.to_be_bytes());
        packet.extend_from_slice(&[0xbe, 0xde, 0, 1]);
        let mut body = vec![0x10, 0x7f, 0, 0];
        body.extend_from_slice(frame);
        let counter = self.counter.to_be_bytes();
        let sealed = cipher.seal(self.counter, &packet, &body).ok()?;
        packet.extend_from_slice(&sealed);
        packet.extend_from_slice(&counter);
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(960);
        self.counter = self.counter.wrapping_add(1);
        Some(packet)
    }
}

/// `(ssrc, frame)` of an RTP packet, opened; `None` for anything else.
pub fn open(cipher: &Cipher, packet: &[u8]) -> Option<(u32, Vec<u8>)> {
    let first = *packet.first()?;
    if first >> 6 != 2 || packet.get(1)? & 0x7f != 0x78 {
        return None;
    }
    let mut header = 12_usize.checked_add(usize::from(first & 0x0f).checked_mul(4)?)?;
    let mut words = 0_usize;
    if first & 0x10 != 0 {
        let preamble = packet.get(header..header.checked_add(4)?)?;
        words = usize::from(u16::from_be_bytes([*preamble.get(2)?, *preamble.get(3)?]));
        header = header.checked_add(4)?;
    }
    let end = packet.len().checked_sub(4)?;
    let counter: [u8; 4] = packet.get(end..)?.try_into().ok()?;
    let mut plain = cipher
        .open(
            u32::from_be_bytes(counter),
            packet.get(..header)?,
            packet.get(header..end)?,
        )
        .ok()?;
    let skip = words.checked_mul(4)?;
    if skip > plain.len() {
        return None;
    }
    plain.drain(..skip);
    let ssrc = u32::from_be_bytes(packet.get(8..12)?.try_into().ok()?);
    Some((ssrc, plain))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    #[test]
    fn packets_round_trip_with_their_extension_stripped() {
        for mode in [AES_GCM, XCHACHA] {
            let cipher = Cipher::new(mode, &[3; 32]).unwrap();
            let mut stream = Stream::new(9, [0; 10]);
            let packet = stream.packet(&cipher, b"hello").unwrap();
            assert_eq!(open(&cipher, &packet).unwrap(), (9, b"hello".to_vec()));
        }
    }
}
