//! Wi-Fi Display information elements.
//!
//! A WFD device announces itself in its P2P frames with a vendor information
//! element whose body is a list of subelements: one byte of ID, two bytes of
//! big-endian length, then the body. NetworkManager and `wpa_supplicant` take
//! and report that subelement list without the vendor header, so that is what
//! this module reads and writes. A list that still carries the header
//! (`0xdd`, length, OUI `50:6f:9a`, type `0x0a`) is accepted too.

/// The RTSP port a WFD device listens on unless its device information says
/// otherwise.
pub const DEFAULT_RTSP_PORT: u16 = 7236;

/// Subelement ID of the WFD Device Information subelement.
pub const DEVICE_INFORMATION: u8 = 0;

const WFA_VENDOR_HEADER: [u8; 4] = [0x50, 0x6f, 0x9a, 0x0a];

/// What a WFD device is, from bits 1:0 of its device information.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceRole {
    /// Sends a stream: a phone, a laptop.
    Source,
    /// Shows a stream: a TV, an adapter.
    PrimarySink,
    /// The second half of a coupled sink pair.
    SecondarySink,
    /// Either, as the session decides.
    DualRole,
}

impl DeviceRole {
    fn bits(self) -> u16 {
        match self {
            Self::Source => 0b00,
            Self::PrimarySink => 0b01,
            Self::SecondarySink => 0b10,
            Self::DualRole => 0b11,
        }
    }

    fn from_bits(bits: u16) -> Self {
        match bits & 0b11 {
            0b00 => Self::Source,
            0b01 => Self::PrimarySink,
            0b10 => Self::SecondarySink,
            _ => Self::DualRole,
        }
    }

    /// Whether a device in this role can show a stream.
    #[must_use]
    pub fn is_sink(self) -> bool {
        !matches!(self, Self::Source)
    }
}

/// The WFD Device Information subelement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Source, sink or both.
    pub role: DeviceRole,
    /// Whether the device accepts a new session now.
    pub session_available: bool,
    /// The TCP port its RTSP endpoint listens on.
    pub rtsp_port: u16,
    /// The highest throughput it supports, in Mbit/s.
    pub max_throughput_mbps: u16,
}

impl DeviceInfo {
    /// A source that is available for a session and serves RTSP on
    /// `rtsp_port`.
    #[must_use]
    pub fn source(rtsp_port: u16) -> Self {
        Self {
            role: DeviceRole::Source,
            session_available: true,
            rtsp_port,
            max_throughput_mbps: 200,
        }
    }

    /// The subelement, header included.
    #[must_use]
    pub fn encode(&self) -> [u8; 9] {
        // Bits 5:4 are the session availability; 0b01 means available.
        let bitmap = self.role.bits() | if self.session_available { 1 << 4 } else { 0 };
        let [b0, b1] = bitmap.to_be_bytes();
        let [p0, p1] = self.rtsp_port.to_be_bytes();
        let [t0, t1] = self.max_throughput_mbps.to_be_bytes();
        [DEVICE_INFORMATION, 0, 6, b0, b1, p0, p1, t0, t1]
    }

    /// Reads a Device Information subelement body (without its header).
    #[must_use]
    pub fn decode(body: &[u8]) -> Option<Self> {
        let [b0, b1, p0, p1, t0, t1, ..] = *body else {
            return None;
        };
        let bitmap = u16::from_be_bytes([b0, b1]);
        Some(Self {
            role: DeviceRole::from_bits(bitmap),
            session_available: (bitmap >> 4) & 0b11 == 0b01,
            rtsp_port: u16::from_be_bytes([p0, p1]),
            max_throughput_mbps: u16::from_be_bytes([t0, t1]),
        })
    }
}

/// The subelements of `ies` as `(id, body)`, stopping at the first one that
/// runs past the end.
pub fn subelements(ies: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut rest = strip_vendor_header(ies);
    std::iter::from_fn(move || {
        let [id, l0, l1, tail @ ..] = rest else {
            return None;
        };
        let length = usize::from(u16::from_be_bytes([*l0, *l1]));
        if tail.len() < length {
            rest = &[];
            return None;
        }
        let (body, after) = tail.split_at(length);
        rest = after;
        Some((*id, body))
    })
}

fn strip_vendor_header(ies: &[u8]) -> &[u8] {
    match ies {
        [0xdd, length, header @ ..]
            if header.starts_with(&WFA_VENDOR_HEADER) && usize::from(*length) <= header.len() =>
        {
            &header[WFA_VENDOR_HEADER.len()..usize::from(*length)]
        }
        _ => ies,
    }
}

/// The Device Information subelement of `ies`, if it has a valid one.
#[must_use]
pub fn device_info(ies: &[u8]) -> Option<DeviceInfo> {
    subelements(ies)
        .find(|(id, _)| *id == DEVICE_INFORMATION)
        .and_then(|(_, body)| DeviceInfo::decode(body))
}

/// The information elements a source announces: its device information.
#[must_use]
pub fn source_ies(rtsp_port: u16) -> Vec<u8> {
    DeviceInfo::source(rtsp_port).encode().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_announces_an_available_source_on_its_rtsp_port() {
        let ies = source_ies(DEFAULT_RTSP_PORT);
        assert_eq!(ies, [0x00, 0x00, 0x06, 0x00, 0x10, 0x1c, 0x44, 0x00, 0xc8]);
        let info = device_info(&ies).expect("device info");
        assert_eq!(info.role, DeviceRole::Source);
        assert!(info.session_available);
        assert_eq!(info.rtsp_port, 7236);
    }

    #[test]
    fn a_sink_port_is_read_from_its_ies_with_or_without_the_vendor_header() {
        // A TV: primary sink, available, RTSP on 7250, then an associated
        // BSSID subelement the parser skips over.
        let subelements = [
            0x01, 0x00, 0x06, 1, 2, 3, 4, 5, 6, // associated BSSID first
            0x00, 0x00, 0x06, 0x00, 0x11, 0x1c, 0x52, 0x00, 0x32,
        ];
        let info = device_info(&subelements).expect("device info");
        assert_eq!(info.role, DeviceRole::PrimarySink);
        assert!(info.role.is_sink());
        assert_eq!(info.rtsp_port, 7250);
        assert_eq!(info.max_throughput_mbps, 50);

        let mut framed = vec![0xdd, (4 + subelements.len()) as u8];
        framed.extend_from_slice(&WFA_VENDOR_HEADER);
        framed.extend_from_slice(&subelements);
        assert_eq!(device_info(&framed), Some(info));
    }

    #[test]
    fn truncated_subelements_are_not_read_past_their_end() {
        assert_eq!(device_info(&[0x00, 0x00, 0x06, 0x00, 0x11]), None);
        assert_eq!(subelements(&[0x00, 0xff, 0xff, 1]).count(), 0);
        assert_eq!(device_info(&[]), None);
    }
}
