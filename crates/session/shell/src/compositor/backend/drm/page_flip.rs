//! KMS submission cookies retained through the kernel event stream.
//!
//! drm-rs's legacy wrapper uses the CRTC as `user_data` and drops that field
//! during decoding. That identifies an output, not the buffer submission.
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use smithay::reexports::drm::control::{crtc, framebuffer};

static NEXT_COOKIE: AtomicU64 = AtomicU64::new(1);
// Linux UAPI DRM_IOCTL_MODE_PAGE_FLIP: _IOWR('d', 0xB0, 24-byte request).
const PAGE_FLIP: libc::Ioctl = 0xc018_64b0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    pub crtc: crtc::Handle,
    pub cookie: u64,
    pub queued_at: Option<Duration>,
}

impl Identity {
    pub fn matches(self, event: &Completion) -> bool {
        self.cookie != 0 && self.cookie == event.cookie && self.crtc == event.crtc
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Completion {
    pub crtc: crtc::Handle,
    pub cookie: u64,
    pub frame: u32,
    pub duration: Duration,
}

#[repr(C)]
struct Request {
    crtc_id: u32,
    fb_id: u32,
    flags: u32,
    reserved: u32,
    user_data: u64,
}

/// Reserve a never-reused cookie. The caller may retain this identity only
/// after KMS accepts the request; rejected attempts consume their cookie too.
pub(super) fn new_identity(crtc: crtc::Handle) -> io::Result<Identity> {
    let cookie = NEXT_COOKIE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| io::Error::other("DRM submission identity exhausted"))?;
    Ok(Identity {
        crtc,
        cookie,
        queued_at: monotonic_now(),
    })
}

pub(super) fn submit_legacy(
    fd: BorrowedFd<'_>,
    crtc: crtc::Handle,
    framebuffer: framebuffer::Handle,
    cookie: u64,
) -> io::Result<()> {
    let mut request = Request {
        crtc_id: crtc.into(),
        fb_id: framebuffer.into(),
        flags: 1, // DRM_MODE_PAGE_FLIP_EVENT; no asynchronous or target-sequence flags.
        reserved: 0,
        user_data: cookie,
    };
    // SAFETY: Request matches drm_mode_crtc_page_flip's UAPI layout; the
    // borrowed seat-owned fd and writable request remain valid for the ioctl.
    if unsafe { libc::ioctl(fd.as_raw_fd(), PAGE_FLIP, &raw mut request) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// Linux UAPI drm_mode_atomic: _IOWR('d', 0xBC, 56-byte request).
const ATOMIC: libc::Ioctl = 0xc038_64bc;

#[repr(C)]
struct AtomicRequest {
    flags: u32,
    count_objs: u32,
    objs_ptr: u64,
    count_props_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    reserved: u64,
    user_data: u64,
}

/// Commit a single primary plane without losing the 64-bit event cookie.
/// drm-rs's atomic wrapper does not expose `user_data`.
pub(super) fn submit_atomic(
    fd: BorrowedFd<'_>,
    plane: u32,
    properties: &[(u32, u64)],
    cookie: u64,
) -> io::Result<()> {
    let count = u32::try_from(properties.len()).map_err(|_| malformed())?;
    let props: Vec<_> = properties.iter().map(|&(prop, _)| prop).collect();
    let values: Vec<_> = properties.iter().map(|&(_, value)| value).collect();
    let mut request = AtomicRequest {
        flags: 1 | 0x0200, // DRM_MODE_PAGE_FLIP_EVENT | DRM_MODE_ATOMIC_NONBLOCK.
        count_objs: 1,
        objs_ptr: (&raw const plane) as usize as u64,
        count_props_ptr: (&raw const count) as usize as u64,
        props_ptr: props.as_ptr() as usize as u64,
        prop_values_ptr: values.as_ptr() as usize as u64,
        reserved: 0,
        user_data: cookie,
    };
    // SAFETY: AtomicRequest matches drm_mode_atomic's UAPI layout. Every
    // referenced array and the borrowed seat fd remain live for the ioctl.
    if unsafe { libc::ioctl(fd.as_raw_fd(), ATOMIC, &raw mut request) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn monotonic_now() -> Option<Duration> {
    let ns = crate::compositor::client_input::monotonic_time_ns()?;
    Some(Duration::from_nanos(u64::try_from(ns).ok()?))
}

pub(super) fn receive(fd: BorrowedFd<'_>) -> io::Result<Vec<Completion>> {
    let mut bytes = [0_u8; 4096];
    // SAFETY: bytes is writable for its entire stated length and fd is live.
    let count = unsafe { libc::read(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    let count = usize::try_from(count).map_err(|_| malformed())?;
    decode(bytes.get(..count).ok_or_else(malformed)?)
}

fn malformed() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid DRM completion event")
}

fn word(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let field = bytes
        .get(offset..offset.checked_add(4).ok_or_else(malformed)?)
        .ok_or_else(malformed)?;
    Ok(u32::from_ne_bytes(
        field.try_into().map_err(|_| malformed())?,
    ))
}

fn decode(mut bytes: &[u8]) -> io::Result<Vec<Completion>> {
    let mut events = Vec::new();
    while !bytes.is_empty() {
        let kind = word(bytes, 0)?;
        let length = usize::try_from(word(bytes, 4)?).map_err(|_| malformed())?;
        if length < 8 || length > bytes.len() {
            return Err(malformed());
        }
        let event = bytes.get(..length).ok_or_else(malformed)?;
        if kind == 2 {
            // DRM_EVENT_FLIP_COMPLETE, drm_event_vblank.
            if length != 32 {
                return Err(malformed());
            }
            let cookie = u64::from_ne_bytes(
                event
                    .get(8..16)
                    .ok_or_else(malformed)?
                    .try_into()
                    .map_err(|_| malformed())?,
            );
            let micros = word(event, 20)?;
            if micros >= 1_000_000 {
                return Err(malformed());
            }
            // A cookie must never be mistaken for a CRTC on old kernels.
            // Unsupported missing CRTC identity stays unavailable.
            if let Some(crtc) = std::num::NonZeroU32::new(word(event, 28)?) {
                events.push(Completion {
                    crtc: crtc.into(),
                    cookie,
                    frame: word(event, 24)?,
                    duration: Duration::new(
                        u64::from(word(event, 16)?),
                        micros.checked_mul(1000).ok_or_else(malformed)?,
                    ),
                });
            }
        }
        bytes = bytes.get(length..).ok_or_else(malformed)?;
    }
    Ok(events)
}

/// Retire only the exact accepted submission, once. A late event does not
/// consume the current target or make its buffer available for rendering.
pub(super) fn consume_completion(identity: &mut Option<Identity>, event: &Completion) -> bool {
    if identity.is_some_and(|pending| pending.matches(event)) {
        *identity = None;
        true
    } else {
        false
    }
}

pub(super) fn unique_completion(
    identity: Option<Identity>,
    events: &[Completion],
) -> Option<Completion> {
    let identity = identity?;
    let mut matching = events.iter().filter(|event| identity.matches(event));
    let event = *matching.next()?;
    matching.next().is_none().then_some(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;

    fn event(cookie: u64, crtc: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(2_u32.to_ne_bytes());
        bytes.extend(32_u32.to_ne_bytes());
        bytes.extend(cookie.to_ne_bytes());
        for word in [123_u32, 456, 789, crtc] {
            bytes.extend(word.to_ne_bytes());
        }
        bytes
    }

    #[test]
    fn actual_event_stream_retains_cookie_timestamp_sequence_and_output() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let cookie = u64::from(u32::MAX) + 17;
        writer.write_all(&event(cookie, 11)).unwrap();
        let events = receive(reader.as_fd()).unwrap();
        let identity = Identity {
            crtc: std::num::NonZeroU32::new(11).unwrap().into(),
            cookie,
            queued_at: Some(Duration::ZERO),
        };
        let completion = unique_completion(Some(identity), &events).unwrap();
        assert_eq!(completion.cookie, cookie);
        assert_eq!(completion.frame, 789);
        assert_eq!(completion.duration, Duration::new(123, 456_000));
        assert_eq!(completion.crtc, identity.crtc);
        let proof = super::super::physical_flip_proof(
            super::super::scanout::DirectRetirement::Complete,
            &events,
            identity.crtc,
            true,
            Some(identity),
        )
        .unwrap();
        assert_eq!(proof.timestamp, completion.duration);
        assert_eq!(proof.sequence, u64::from(completion.frame));
    }

    #[test]
    fn replaced_cookie_foreign_output_duplicate_and_missing_identity_fail_closed() {
        let events = decode(&event(7, 11)).unwrap();
        let own = Identity {
            crtc: events[0].crtc,
            cookie: 7,
            queued_at: Some(Duration::ZERO),
        };
        assert!(unique_completion(Some(own), &events).is_some());
        assert!(unique_completion(Some(Identity { cookie: 8, ..own }), &events).is_none());
        let foreign = Identity {
            crtc: std::num::NonZeroU32::new(12).unwrap().into(),
            ..own
        };
        assert!(unique_completion(Some(foreign), &events).is_none());
        assert!(unique_completion(None, &events).is_none());
        assert!(unique_completion(Some(own), &[events[0], events[0]]).is_none());
        assert!(decode(&event(7, 0)).unwrap().is_empty());
    }

    #[test]
    fn real_decoded_late_completion_cannot_release_replacement_and_each_target_retires_once() {
        let late = decode(&event(7, 11)).unwrap()[0];
        let current = decode(&event(8, 11)).unwrap()[0];
        let mut pending = Some(Identity {
            crtc: current.crtc,
            cookie: current.cookie,
            queued_at: Some(Duration::ZERO),
        });
        assert!(!consume_completion(&mut pending, &late));
        assert_eq!(pending.unwrap().cookie, 8);
        assert!(consume_completion(&mut pending, &current));
        assert!(pending.is_none());
        assert!(!consume_completion(&mut pending, &current));
    }

    #[test]
    fn malformed_event_batch_never_returns_partial_success() {
        let mut bytes = event(7, 11);
        bytes.extend([2, 0, 0]);
        assert!(decode(&bytes).is_err());
        let mut bad_time = event(7, 11);
        bad_time[20..24].copy_from_slice(&1_000_000_u32.to_ne_bytes());
        assert!(decode(&bad_time).is_err());
        let mut bad_length = event(7, 11);
        bad_length[4..8].copy_from_slice(&0_u32.to_ne_bytes());
        assert!(decode(&bad_length).is_err());
    }

    #[test]
    fn refused_real_ioctl_cannot_establish_pending_submission() {
        assert_eq!(std::mem::size_of::<Request>(), 24);
        let (socket, _) = UnixStream::pair().unwrap();
        let crtc = std::num::NonZeroU32::new(11).unwrap().into();
        let fb = std::num::NonZeroU32::new(12).unwrap().into();
        let identity = new_identity(crtc).unwrap();
        assert!(submit_legacy(socket.as_fd(), crtc, fb, identity.cookie).is_err());
        assert_eq!(std::mem::size_of::<AtomicRequest>(), 56);
        assert!(submit_atomic(socket.as_fd(), 13, &[(14, 12)], identity.cookie).is_err());
        assert_ne!(new_identity(crtc).unwrap().cookie, identity.cookie);
    }
}
