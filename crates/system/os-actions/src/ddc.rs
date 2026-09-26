//! External-monitor brightness over DDC/CI (VESA MCCS VCP code `0x10`).
//!
//! A monitor answers DDC/CI on I²C address `0x37` of the bus its connector
//! carries (`/sys/class/drm/<card>-<connector>/ddc`, or a DP AUX adapter
//! beneath the connector). Linux exposes those buses as `/dev/i2c-N`
//! through the `i2c-dev` module; Tilo loads it and grants the session user
//! access with a udev rule (`os/usr/lib/udev/rules.d/`), so this is a few
//! `write`/`read` calls rather than a `ddcutil` process per slider step.
//!
//! Monitors are slow and forgetful: they need ~40 ms before a reply is
//! ready and ~50 ms after a write before they accept the next command, and
//! some answer a busy request with a null message or a garbled reply. Every
//! exchange is checksummed and retried a bounded number of times; callers
//! must keep this off any latency-sensitive thread (see
//! [`crate::brightness::BrightnessService`]).

use std::io;
use std::time::Duration;

/// DDC/CI slave address (7-bit) every monitor answers on.
pub const DDC_ADDRESS: u16 = 0x37;
/// MCCS "Luminance".
pub const VCP_BRIGHTNESS: u8 = 0x10;

/// 8-bit write address of the display, the checksum seed for requests.
const DISPLAY_WRITE_ADDRESS: u8 = 0x6E;
/// Host "virtual source" address, the checksum seed for replies.
const HOST_VIRTUAL_ADDRESS: u8 = 0x50;
/// Source-address byte a host puts first in every request.
const HOST_SOURCE: u8 = 0x51;

const GET_VCP: u8 = 0x01;
const GET_VCP_REPLY: u8 = 0x02;
const SET_VCP: u8 = 0x03;

const REPLY_DELAY: Duration = Duration::from_millis(40);
const WRITE_SETTLE: Duration = Duration::from_millis(50);
const RETRY_DELAY: Duration = Duration::from_millis(60);
const ATTEMPTS: usize = 3;

/// The byte-level channel to one monitor. The real one is an `i2c-dev`
/// node; tests substitute a scripted fake.
pub trait I2cTransport {
    /// # Errors
    ///
    /// Returns the bus error; the caller retries.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// # Errors
    ///
    /// Returns the bus error; the caller retries.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<()>;
    /// Wait for the monitor. Real transports sleep; fakes return at once.
    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// A VCP feature's current and maximum value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VcpValue {
    pub current: u16,
    pub maximum: u16,
}

impl VcpValue {
    /// As a `0.0..=1.0` fraction; `None` when the monitor reports no range.
    pub fn fraction(self) -> Option<f32> {
        (self.maximum > 0).then(|| (f32::from(self.current) / f32::from(self.maximum)).min(1.0))
    }
}

/// XOR checksum over `seed` and `bytes`.
pub fn checksum(seed: u8, bytes: &[u8]) -> u8 {
    bytes.iter().fold(seed, |acc, b| acc ^ b)
}

/// Frame a request payload: source, length, data, checksum.
pub fn request(data: &[u8]) -> Vec<u8> {
    let length = u8::try_from(data.len()).unwrap_or(0x7F) & 0x7F;
    let mut frame = Vec::with_capacity(data.len().saturating_add(3));
    frame.push(HOST_SOURCE);
    frame.push(0x80 | length);
    frame.extend_from_slice(data);
    frame.push(checksum(DISPLAY_WRITE_ADDRESS, &frame));
    frame
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReplyError {
    /// The monitor sent the null message: busy, try again.
    Busy,
    BadChecksum,
    Malformed,
    /// The monitor says it does not implement this VCP code.
    Unsupported,
}

/// Parse a Get VCP Feature reply (`read` of 11 bytes).
///
/// # Errors
///
/// Returns why the reply cannot be trusted.
pub fn parse_get_vcp_reply(reply: &[u8], vcp: u8) -> Result<VcpValue, ReplyError> {
    // Null message: source, 0x80, checksum.
    if reply.get(1) == Some(&0x80) {
        return Err(ReplyError::Busy);
    }
    let frame: &[u8; 11] = reply
        .get(..11)
        .and_then(|frame| frame.try_into().ok())
        .ok_or(ReplyError::Malformed)?;
    let (body, sum) = frame.split_at(10);
    if sum.first() != Some(&checksum(HOST_VIRTUAL_ADDRESS, body)) {
        return Err(ReplyError::BadChecksum);
    }
    let &[
        _source,
        length,
        opcode,
        result,
        code,
        _kind,
        max_hi,
        max_lo,
        cur_hi,
        cur_lo,
    ] = body
    else {
        return Err(ReplyError::Malformed);
    };
    if length != 0x88 || opcode != GET_VCP_REPLY || code != vcp {
        return Err(ReplyError::Malformed);
    }
    if result != 0 {
        return Err(ReplyError::Unsupported);
    }
    Ok(VcpValue {
        current: u16::from_be_bytes([cur_hi, cur_lo]),
        maximum: u16::from_be_bytes([max_hi, max_lo]),
    })
}

/// DDC/CI error, with the last cause after all retries.
#[derive(Debug, thiserror::Error)]
pub enum DdcError {
    #[error("DDC/CI bus error: {0}")]
    Io(#[from] io::Error),
    #[error("monitor did not give a valid DDC/CI reply ({0:?})")]
    Reply(ReplyError),
}

/// Read a VCP feature, retrying busy or garbled replies.
///
/// # Errors
///
/// Returns the last failure once every attempt failed.
pub fn get_vcp(transport: &mut dyn I2cTransport, vcp: u8) -> Result<VcpValue, DdcError> {
    let frame = request(&[GET_VCP, vcp]);
    let mut last = DdcError::Reply(ReplyError::Malformed);
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            transport.pause(RETRY_DELAY);
        }
        if let Err(error) = transport.write(&frame) {
            last = error.into();
            continue;
        }
        transport.pause(REPLY_DELAY);
        let mut reply = [0u8; 11];
        if let Err(error) = transport.read(&mut reply) {
            last = error.into();
            continue;
        }
        match parse_get_vcp_reply(&reply, vcp) {
            Ok(value) => return Ok(value),
            // Retrying cannot make a monitor support a feature.
            Err(ReplyError::Unsupported) => {
                return Err(DdcError::Reply(ReplyError::Unsupported));
            }
            Err(error) => last = DdcError::Reply(error),
        }
    }
    Err(last)
}

/// Write a VCP feature. DDC/CI has no acknowledgement for a set, so only
/// bus errors are retried; the settle pause keeps the next command from
/// arriving before the monitor is ready.
///
/// # Errors
///
/// Returns the last bus error once every attempt failed.
pub fn set_vcp(transport: &mut dyn I2cTransport, vcp: u8, value: u16) -> Result<(), DdcError> {
    let [hi, lo] = value.to_be_bytes();
    let frame = request(&[SET_VCP, vcp, hi, lo]);
    let mut last = None;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            transport.pause(RETRY_DELAY);
        }
        match transport.write(&frame) {
            Ok(()) => {
                transport.pause(WRITE_SETTLE);
                return Ok(());
            }
            Err(error) => last = Some(error),
        }
    }
    Err(last.map_or(DdcError::Reply(ReplyError::Malformed), DdcError::Io))
}

/// An `i2c-dev` node bound to the DDC/CI address.
#[cfg(target_os = "linux")]
pub struct I2cDevice {
    file: std::fs::File,
}

#[cfg(target_os = "linux")]
impl I2cDevice {
    /// `I2C_SLAVE` from `linux/i2c-dev.h`.
    const I2C_SLAVE: libc::c_ulong = 0x0703;

    /// # Errors
    ///
    /// Returns the error from opening the node or selecting the address.
    pub fn open(path: &std::path::Path) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        // SAFETY: `file` is an open descriptor for the whole call; I2C_SLAVE
        // takes the 7-bit address by value and touches no memory of ours.
        let rc = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                Self::I2C_SLAVE,
                libc::c_ulong::from(DDC_ADDRESS),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file })
    }
}

#[cfg(target_os = "linux")]
impl I2cTransport for I2cDevice {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;
        self.file.write_all(bytes)
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<()> {
        use std::io::Read;
        self.file.read_exact(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Scripted monitor: records writes, replays reads.
    #[derive(Default)]
    struct FakeMonitor {
        written: Vec<Vec<u8>>,
        replies: VecDeque<io::Result<Vec<u8>>>,
        write_failures: usize,
        paused: Duration,
    }

    impl I2cTransport for FakeMonitor {
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            if self.write_failures > 0 {
                self.write_failures = self.write_failures.saturating_sub(1);
                return Err(io::Error::other("NAK"));
            }
            self.written.push(bytes.to_vec());
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8]) -> io::Result<()> {
            let reply = self
                .replies
                .pop_front()
                .unwrap_or_else(|| Err(io::Error::other("no reply")))?;
            buf.copy_from_slice(&reply[..buf.len()]);
            Ok(())
        }

        fn pause(&mut self, duration: Duration) {
            self.paused = self.paused.saturating_add(duration);
        }
    }

    fn reply(current: u16, maximum: u16) -> Vec<u8> {
        let [max_hi, max_lo] = maximum.to_be_bytes();
        let [cur_hi, cur_lo] = current.to_be_bytes();
        let mut body = vec![
            0x6E, 0x88, 0x02, 0x00, 0x10, 0x00, max_hi, max_lo, cur_hi, cur_lo,
        ];
        body.push(checksum(0x50, &body));
        body
    }

    #[test]
    fn requests_carry_the_mccs_frame_and_checksum() {
        // ddcutil's well-known "get brightness" frame.
        assert_eq!(request(&[0x01, 0x10]), vec![0x51, 0x82, 0x01, 0x10, 0xAC]);
        let set = request(&[0x03, 0x10, 0x00, 0x32]);
        assert_eq!(&set[..6], &[0x51, 0x84, 0x03, 0x10, 0x00, 0x32]);
        assert_eq!(set[6], checksum(0x6E, &set[..6]));
    }

    #[test]
    fn a_reply_yields_current_and_maximum() {
        let value = parse_get_vcp_reply(&reply(70, 100), 0x10).unwrap();
        assert_eq!(
            value,
            VcpValue {
                current: 70,
                maximum: 100
            }
        );
        assert_eq!(value.fraction(), Some(0.7));
    }

    #[test]
    fn corrupt_null_and_unsupported_replies_are_told_apart() {
        let mut corrupt = reply(70, 100);
        corrupt[8] ^= 0x01;
        assert_eq!(
            parse_get_vcp_reply(&corrupt, 0x10),
            Err(ReplyError::BadChecksum)
        );
        let null = [0x6E, 0x80, 0xBE, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse_get_vcp_reply(&null, 0x10), Err(ReplyError::Busy));
        let mut unsupported = reply(0, 0);
        unsupported[3] = 0x01;
        unsupported[10] = checksum(0x50, &unsupported[..10]);
        assert_eq!(
            parse_get_vcp_reply(&unsupported, 0x10),
            Err(ReplyError::Unsupported)
        );
        let mut other_code = reply(70, 100);
        other_code[4] = 0x12;
        other_code[10] = checksum(0x50, &other_code[..10]);
        assert_eq!(
            parse_get_vcp_reply(&other_code, 0x10),
            Err(ReplyError::Malformed)
        );
    }

    #[test]
    fn a_busy_monitor_is_asked_again() {
        let mut corrupt = reply(40, 100);
        corrupt[10] ^= 0xFF;
        let mut monitor = FakeMonitor {
            replies: VecDeque::from([Ok(corrupt), Ok(reply(40, 100))]),
            ..FakeMonitor::default()
        };
        let value = get_vcp(&mut monitor, VCP_BRIGHTNESS).unwrap();
        assert_eq!(value.current, 40);
        assert_eq!(monitor.written.len(), 2, "one retry");
        assert!(monitor.paused >= REPLY_DELAY * 2, "waited for each reply");
    }

    #[test]
    fn retries_are_bounded() {
        let mut monitor = FakeMonitor::default();
        assert!(get_vcp(&mut monitor, VCP_BRIGHTNESS).is_err());
        assert_eq!(monitor.written.len(), ATTEMPTS);
    }

    #[test]
    fn an_unsupported_feature_is_not_retried() {
        let mut unsupported = reply(0, 0);
        unsupported[3] = 0x01;
        unsupported[10] = checksum(0x50, &unsupported[..10]);
        let mut monitor = FakeMonitor {
            replies: VecDeque::from([Ok(unsupported)]),
            ..FakeMonitor::default()
        };
        assert!(matches!(
            get_vcp(&mut monitor, VCP_BRIGHTNESS),
            Err(DdcError::Reply(ReplyError::Unsupported))
        ));
        assert_eq!(monitor.written.len(), 1);
    }

    #[test]
    fn a_set_survives_a_nak_and_waits_for_the_monitor_to_settle() {
        let mut monitor = FakeMonitor {
            write_failures: 1,
            ..FakeMonitor::default()
        };
        set_vcp(&mut monitor, VCP_BRIGHTNESS, 55).unwrap();
        assert_eq!(monitor.written, vec![request(&[0x03, 0x10, 0x00, 55])]);
        assert!(monitor.paused >= WRITE_SETTLE);
    }
}
