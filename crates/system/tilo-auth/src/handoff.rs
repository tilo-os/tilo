//! The boot sign-in handed from the initrd to the session: a kernel key of
//! type `user`, described `tilo-signin`, in uid 0's user keyring, readable
//! and viewable by possessor and owner only and gone after ten minutes. Its
//! payload is the person's uid (4 bytes, little-endian) and their user key.
//! It never touches disk; `tilo-authd` reads and unlinks it when it starts.

use crate::userkey::{KEY_LEN, UserKey};
use std::ffi::{CStr, CString, c_long};
use std::io;
use zeroize::Zeroizing;

pub const DESCRIPTION: &str = "tilo-signin";
pub const TIMEOUT_S: u32 = 600;
const PAYLOAD_LEN: usize = 4 + KEY_LEN;

const KEY_SPEC_USER_KEYRING: i32 = -4;
const KEYCTL_SETPERM: c_long = 5;
const KEYCTL_DESCRIBE: c_long = 6;
const KEYCTL_UNLINK: c_long = 9;
const KEYCTL_READ: c_long = 11;
const KEYCTL_SET_TIMEOUT: c_long = 15;
const KEY_POS_VIEW: u32 = 0x0100_0000;
const KEY_POS_READ: u32 = 0x0200_0000;
const KEY_USR_VIEW: u32 = 0x0001_0000;
const KEY_USR_READ: u32 = 0x0002_0000;

pub struct Handoff {
    pub uid: u32,
    pub key: UserKey,
}

impl Handoff {
    pub fn encode(&self) -> Zeroizing<Vec<u8>> {
        let mut out = Zeroizing::new(Vec::with_capacity(PAYLOAD_LEN));
        out.extend_from_slice(&self.uid.to_le_bytes());
        out.extend_from_slice(self.key.expose());
        out
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() != PAYLOAD_LEN {
            return None;
        }
        let (uid, key) = payload.split_at(4);
        Some(Self {
            uid: u32::from_le_bytes(uid.try_into().ok()?),
            key: UserKey::from_slice(key)?,
        })
    }
}

fn check(ret: c_long) -> io::Result<c_long> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn keyctl(op: c_long, a: c_long, b: c_long, c: c_long) -> io::Result<c_long> {
    // SAFETY: keyctl takes integer arguments for these operations, or a
    // pointer and length the callers below keep valid for the call.
    check(unsafe { libc::syscall(libc::SYS_keyctl, op, a, b, c) })
}

fn serials_in_user_keyring() -> io::Result<Vec<i32>> {
    let mut buf = vec![0_i32; 64];
    loop {
        let bytes = std::mem::size_of_val(buf.as_slice());
        let needed = keyctl(
            KEYCTL_READ,
            c_long::from(KEY_SPEC_USER_KEYRING),
            buf.as_mut_ptr() as c_long,
            c_long::try_from(bytes).unwrap_or(c_long::MAX),
        )?;
        let needed = usize::try_from(needed).unwrap_or(0);
        if needed <= bytes {
            buf.truncate(needed.checked_div(std::mem::size_of::<i32>()).unwrap_or(0));
            return Ok(buf);
        }
        buf.resize(
            needed
                .checked_div(std::mem::size_of::<i32>())
                .unwrap_or(0)
                .saturating_add(1),
            0,
        );
    }
}

/// `(type, uid, description)` of a key this process may view.
fn describe(serial: i32) -> Option<(String, u32, String)> {
    let mut buf = vec![0_u8; 512];
    let len = keyctl(
        KEYCTL_DESCRIBE,
        c_long::from(serial),
        buf.as_mut_ptr() as c_long,
        c_long::try_from(buf.len()).unwrap_or(0),
    )
    .ok()?;
    buf.truncate(usize::try_from(len).ok()?.min(buf.len()));
    let text = CStr::from_bytes_until_nul(&buf).ok()?.to_str().ok()?;
    let mut parts = text.splitn(5, ';');
    let kind = parts.next()?.to_owned();
    let uid = parts.next()?.parse().ok()?;
    let _gid = parts.next()?;
    let _perm = parts.next()?;
    Some((kind, uid, parts.next()?.to_owned()))
}

fn read_payload(serial: i32) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut buf = Zeroizing::new(vec![0_u8; PAYLOAD_LEN + 1]);
    let len = keyctl(
        KEYCTL_READ,
        c_long::from(serial),
        buf.as_mut_ptr() as c_long,
        c_long::try_from(buf.len()).unwrap_or(0),
    )?;
    let len = usize::try_from(len).unwrap_or(usize::MAX).min(buf.len());
    buf.truncate(len);
    Ok(buf)
}

fn unlink(serial: i32) {
    let _ = keyctl(
        KEYCTL_UNLINK,
        c_long::from(serial),
        c_long::from(KEY_SPEC_USER_KEYRING),
        0,
    );
}

/// Our handoff keys in this uid's user keyring.
fn ours(description: &str) -> io::Result<Vec<i32>> {
    let me = rustix::process::getuid().as_raw();
    Ok(serials_in_user_keyring()?
        .into_iter()
        .filter(|&serial| {
            describe(serial)
                .is_some_and(|(kind, uid, desc)| kind == "user" && uid == me && desc == description)
        })
        .collect())
}

/// Leave `handoff` for the session. Any earlier handoff goes first, so the
/// session never finds a stale one beside it.
pub fn add(handoff: &Handoff, description: &str) -> io::Result<()> {
    for stale in ours(description)? {
        unlink(stale);
    }
    let kind = c"user";
    let desc = CString::new(description).map_err(|_| io::ErrorKind::InvalidInput)?;
    let payload = handoff.encode();
    // SAFETY: every pointer is valid for the call and the length matches the
    // payload buffer.
    let serial = check(unsafe {
        libc::syscall(
            libc::SYS_add_key,
            kind.as_ptr(),
            desc.as_ptr(),
            payload.as_ptr(),
            payload.len(),
            c_long::from(KEY_SPEC_USER_KEYRING),
        )
    })?;
    let set_up = keyctl(KEYCTL_SET_TIMEOUT, serial, c_long::from(TIMEOUT_S), 0).and_then(|_| {
        keyctl(
            KEYCTL_SETPERM,
            serial,
            c_long::from(KEY_POS_VIEW | KEY_POS_READ | KEY_USR_VIEW | KEY_USR_READ),
            0,
        )
    });
    if let Err(e) = set_up {
        unlink(i32::try_from(serial).unwrap_or(0));
        return Err(e);
    }
    Ok(())
}

/// Read and destroy the handoff. More than one is ambiguous and none is used.
pub fn take(description: &str) -> io::Result<Option<Handoff>> {
    let found = ours(description)?;
    let payloads: Vec<_> = found.iter().map(|&s| read_payload(s)).collect();
    for &serial in &found {
        unlink(serial);
    }
    match payloads.as_slice() {
        [Ok(payload)] => Ok(Handoff::decode(payload)),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_is_the_uid_then_the_key() {
        let handoff = Handoff {
            uid: 0x0102_0304,
            key: UserKey::from_bytes([9; KEY_LEN]),
        };
        let bytes = handoff.encode();
        assert_eq!(bytes.len(), 36);
        assert_eq!(&bytes[..4], &[4, 3, 2, 1]);
        assert_eq!(&bytes[4..], &[9; KEY_LEN]);
        let back = Handoff::decode(&bytes).unwrap();
        assert_eq!(back.uid, handoff.uid);
        assert_eq!(back.key.expose(), handoff.key.expose());
        assert!(Handoff::decode(&bytes[..35]).is_none());
    }
}
