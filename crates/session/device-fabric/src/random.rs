//! Fresh bytes from the operating system's generator, for ids and nonces.

use getrandom::SysRng;
use getrandom::rand_core::{Rng as _, UnwrapErr};

pub fn bytes<const N: usize>() -> [u8; N] {
    let mut value = [0; N];
    UnwrapErr(SysRng).fill_bytes(&mut value);
    value
}

/// `N` random bytes, never all zero: the id and nonce formats reserve zero
/// for "none".
pub fn nonzero<const N: usize>() -> [u8; N] {
    loop {
        let value = bytes::<N>();
        if value != [0; N] {
            return value;
        }
    }
}
