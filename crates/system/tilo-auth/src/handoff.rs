//! The boot sign-in handed from the initrd to the session: a kernel key of
//! type `user`, described `tilo-signin`, in uid 0's user keyring, readable
//! and viewable by possessor and owner only and gone after ten minutes. Its
//! payload is the person's uid (4 bytes, little-endian) and their user key.
//! It never touches disk; `tilo-authd` reads and unlinks it when it starts.

use crate::keyring::{
    KEY_POS_READ, KEY_POS_VIEW, KEY_SPEC_PROCESS_KEYRING, KEY_SPEC_USER_KEYRING, KEY_USR_READ,
    KEY_USR_VIEW, KEYCTL_LINK, KEYCTL_SET_TIMEOUT, KEYCTL_SETPERM, check, keyctl, ours,
    read_payload, unlink, unlink_from,
};
use crate::userkey::{KEY_LEN, UserKey};
use std::ffi::{CString, c_long};
use std::io;
use zeroize::Zeroizing;

pub const DESCRIPTION: &str = "tilo-signin";
pub const TIMEOUT_S: u32 = 600;
const PAYLOAD_LEN: usize = 4 + KEY_LEN;

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

/// Leave `handoff` for the session. Any earlier handoff goes first, so the
/// session never finds a stale one beside it.
///
/// The key is made in this process's keyring, where it is possessed: a
/// service's session keyring does not link the user keyring, and a new key
/// grants setting its timeout and permissions to its possessor only. Once
/// set up it is linked into the user keyring and the process keyring lets
/// go of it.
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
            c_long::from(KEY_SPEC_PROCESS_KEYRING),
        )
    })?;
    let set_up = keyctl(KEYCTL_SET_TIMEOUT, serial, c_long::from(TIMEOUT_S), 0)
        .and_then(|_| keyctl(KEYCTL_LINK, serial, c_long::from(KEY_SPEC_USER_KEYRING), 0))
        // Last: the permissions it ends with no longer let anyone link it.
        .and_then(|_| {
            keyctl(
                KEYCTL_SETPERM,
                serial,
                c_long::from(KEY_POS_VIEW | KEY_POS_READ | KEY_USR_VIEW | KEY_USR_READ),
                0,
            )
        });
    let serial = i32::try_from(serial).unwrap_or(0);
    unlink_from(serial, KEY_SPEC_PROCESS_KEYRING);
    if let Err(e) = set_up {
        unlink(serial);
        return Err(e);
    }
    Ok(())
}

/// Read and destroy the handoff. More than one is ambiguous and none is used.
pub fn take(description: &str) -> io::Result<Option<Handoff>> {
    let found = ours(description)?;
    let payloads: Vec<_> = found
        .iter()
        .map(|&s| read_payload(s, PAYLOAD_LEN))
        .collect();
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
