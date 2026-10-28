//! Fresh bytes from the operating system's generator, for ids, nonces,
//! cookies and keys.

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;

pub(crate) fn bytes<const N: usize>() -> [u8; N] {
    let mut value = [0; N];
    UnwrapErr(SysRng).fill_bytes(&mut value);
    value
}

/// `N` random bytes, never all zero: the id and nonce formats reserve zero
/// for "none".
pub(crate) fn nonzero<const N: usize>() -> [u8; N] {
    loop {
        let value = bytes::<N>();
        if value != [0; N] {
            return value;
        }
    }
}
