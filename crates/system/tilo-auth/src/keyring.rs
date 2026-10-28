//! uid 0's user keyring, where the initrd leaves what the booted system
//! needs once: the boot sign-in and the system volume's key.

use std::ffi::{CStr, c_long};
use std::io;
use zeroize::Zeroizing;

pub(crate) const KEY_SPEC_PROCESS_KEYRING: i32 = -2;
pub(crate) const KEY_SPEC_USER_KEYRING: i32 = -4;
pub(crate) const KEYCTL_SETPERM: c_long = 5;
pub(crate) const KEYCTL_DESCRIBE: c_long = 6;
pub(crate) const KEYCTL_LINK: c_long = 8;
pub(crate) const KEYCTL_UNLINK: c_long = 9;
pub(crate) const KEYCTL_READ: c_long = 11;
pub(crate) const KEYCTL_SET_TIMEOUT: c_long = 15;
pub(crate) const KEY_POS_VIEW: u32 = 0x0100_0000;
pub(crate) const KEY_POS_READ: u32 = 0x0200_0000;
pub(crate) const KEY_USR_VIEW: u32 = 0x0001_0000;
pub(crate) const KEY_USR_READ: u32 = 0x0002_0000;

pub(crate) fn check(ret: c_long) -> io::Result<c_long> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

pub(crate) fn keyctl(op: c_long, a: c_long, b: c_long, c: c_long) -> io::Result<c_long> {
    // SAFETY: keyctl takes integer arguments for these operations, or a
    // pointer and length the callers below keep valid for the call.
    check(unsafe { libc::syscall(libc::SYS_keyctl, op, a, b, c) })
}

pub(crate) fn serials_in_user_keyring() -> io::Result<Vec<i32>> {
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
pub(crate) fn describe(serial: i32) -> Option<(String, u32, String)> {
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

pub(crate) fn read_payload(serial: i32, max: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut buf = Zeroizing::new(vec![0_u8; max.saturating_add(1)]);
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

pub(crate) fn unlink(serial: i32) {
    unlink_from(serial, KEY_SPEC_USER_KEYRING);
}

pub(crate) fn unlink_from(serial: i32, keyring: i32) {
    let _ = keyctl(
        KEYCTL_UNLINK,
        c_long::from(serial),
        c_long::from(keyring),
        0,
    );
}

/// Our handoff keys in this uid's user keyring.
pub(crate) fn ours(description: &str) -> io::Result<Vec<i32>> {
    let me = rustix::process::getuid().as_raw();
    Ok(serials_in_user_keyring()?
        .into_iter()
        .filter(|&serial| {
            describe(serial)
                .is_some_and(|(kind, uid, desc)| kind == "user" && uid == me && desc == description)
        })
        .collect())
}

/// The one `user` key described `description`, of at most `max` bytes, left
/// where it is. None when there is none, or more than one.
pub fn read_one(description: &str, max: usize) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    match ours(description)?.as_slice() {
        [serial] => read_payload(*serial, max).map(Some),
        _ => Ok(None),
    }
}

/// Unlink every `user` key described `description`.
pub fn forget(description: &str) -> io::Result<()> {
    for serial in ours(description)? {
        unlink(serial);
    }
    Ok(())
}
