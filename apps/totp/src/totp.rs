//! TOTP (RFC 6238) on the `RustCrypto` `hmac`, `sha1` and `sha2` primitives,
//! with base32 secrets decoded by `data-encoding`.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use zeroize::Zeroizing;

use crate::model::{Account, Algorithm};

/// Unused bits in the last symbol are ignored: authenticator apps accept such
/// secrets.
const SECRET_BASE32: data_encoding::Encoding = data_encoding_macro::new_encoding! {
    symbols: "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567",
    check_trailing_bits: false,
};

pub fn decode_secret(secret: &str) -> Option<Vec<u8>> {
    let normalised = crate::model::normalise_secret(secret);
    if normalised.is_empty() {
        return None;
    }
    decode_base32_lenient(&SECRET_BASE32, normalised.as_bytes())
}

/// Drop a final symbol that holds no whole byte instead of failing.
fn decode_base32_lenient(encoding: &data_encoding::Encoding, symbols: &[u8]) -> Option<Vec<u8>> {
    match encoding.decode(symbols) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind == data_encoding::DecodeKind::Length => {
            let (last, head) = symbols.split_last()?;
            encoding.decode(&[*last, *last]).ok()?;
            encoding.decode(head).ok()
        }
        Err(_) => None,
    }
}

pub fn unix_now() -> f64 {
    ui::time::Duration::from_nanos(ui::time::unix_now_ns()).as_secs_f64()
}

/// `None` only when the HMAC setup fails, which it never does for HMAC (any key
/// length is valid); an `Option` beats an `expect` on secret-derived input. The
/// modulus is `u64` so ten digits cannot overflow.
fn hotp(key: &[u8], counter: u64, digits: u32, algorithm: Algorithm) -> Option<u64> {
    let msg = counter.to_be_bytes();
    let digest: Vec<u8> = match algorithm {
        Algorithm::Sha1 => {
            let mut mac = <Hmac<Sha1>>::new_from_slice(key).ok()?;
            mac.update(&msg);
            mac.finalize().into_bytes().to_vec()
        }
        Algorithm::Sha256 => {
            let mut mac = <Hmac<Sha256>>::new_from_slice(key).ok()?;
            mac.update(&msg);
            mac.finalize().into_bytes().to_vec()
        }
        Algorithm::Sha512 => {
            let mut mac = <Hmac<Sha512>>::new_from_slice(key).ok()?;
            mac.update(&msg);
            mac.finalize().into_bytes().to_vec()
        }
    };

    // RFC 4226 §5.3 dynamic truncation: the low nibble of the last byte picks a
    // 4-byte window; the top bit of its first byte is masked off.
    let last = digest.last().copied().unwrap_or(0);
    let offset = usize::from(last & 0x0f);
    let window: [u8; 4] = digest
        .get(offset..offset + 4)
        .and_then(|w| w.try_into().ok())
        .unwrap_or([0; 4]);
    let bin = ((u32::from(window[0]) & 0x7f) << 24)
        | (u32::from(window[1]) << 16)
        | (u32::from(window[2]) << 8)
        | u32::from(window[3]);
    Some(u64::from(bin) % 10u64.pow(digits))
}

pub fn code_at(account: &Account, unix_secs: f64) -> String {
    let digits = u32::from(account.digits).clamp(4, 10);
    let Some(key) = decode_secret(&account.secret) else {
        return "•".repeat(digits as usize);
    };
    // Wipe the key from the heap once this code is derived.
    let key = Zeroizing::new(key);
    if key.is_empty() {
        return "•".repeat(digits as usize);
    }
    let period = f64::from(account.period.max(1));
    let counter = (unix_secs / period).floor() as u64;
    let Some(value) = hotp(&key, counter, digits, account.algorithm) else {
        return "•".repeat(digits as usize);
    };
    format!("{value:0width$}", width = digits as usize)
}

pub fn current_code(account: &Account) -> String {
    code_at(account, unix_now())
}

/// A warning for a device whose clock is not synchronised: its codes are
/// rejected by every server. `None` when the clock is fine, or when the
/// platform cannot say: a warning nobody can act on is noise.
pub fn clock_drift_warning() -> Option<&'static str> {
    if cfg!(test) || ui::time::wall_frozen() {
        return None;
    }
    match ui::time::is_synchronised() {
        Some(false) => Some(
            "This device\u{2019}s clock is not synchronised; codes may be rejected. \
             Turn on automatic time, then try again.",
        ),
        Some(true) | None => None,
    }
}

pub fn group_code(code: &str) -> String {
    let n = code.chars().count();
    if n == 0 {
        return String::new();
    }
    let split = match n {
        6 => 3,
        8 => 4,
        _ => n.div_ceil(2),
    };
    let chars: Vec<char> = code.chars().collect();
    let (a, b) = chars.split_at(split.min(n));
    if b.is_empty() {
        a.iter().collect()
    } else {
        format!(
            "{}\u{2009}{}",
            a.iter().collect::<String>(),
            b.iter().collect::<String>()
        )
    }
}

pub fn seconds_remaining(account: &Account, unix_secs: f64) -> u32 {
    let period = f64::from(account.period.max(1));
    let within = unix_secs.rem_euclid(period);
    (period - within).ceil().max(1.0) as u32
}

pub fn period_progress(account: &Account, unix_secs: f64) -> f64 {
    let period = f64::from(account.period.max(1));
    let within = unix_secs.rem_euclid(period);
    (1.0 - within / period).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Account;
    use uuid::Uuid;

    fn acc(secret: &str, alg: Algorithm, digits: u8, period: u32) -> Account {
        let mut a = Account::blank(Uuid::new_v4());
        a.secret = crate::model::normalise_secret(secret);
        a.algorithm = alg;
        a.digits = digits;
        a.period = period;
        a
    }

    #[test]
    fn codes_match_rfc6238_and_track_the_clock() {
        {
            let a = acc("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", Algorithm::Sha1, 8, 30);
            assert_eq!(code_at(&a, 59.0), "94287082");
            assert_eq!(code_at(&a, 1_111_111_109.0), "07081804");
            assert_eq!(code_at(&a, 1_234_567_890.0), "89005924");
            assert_eq!(code_at(&a, 2_000_000_000.0), "69279037");
        }

        {
            let a = acc("JBSWY3DPEHPK3PXP", Algorithm::Sha1, 6, 30);
            assert_eq!(code_at(&a, 100.0), code_at(&a, 119.0));
            assert_ne!(code_at(&a, 100.0), code_at(&a, 130.0));
        }

        {
            let a = acc("JBSWY3DPEHPK3PXP", Algorithm::Sha1, 6, 30);
            assert!((period_progress(&a, 0.0) - 1.0).abs() < 1e-9);
            assert_eq!(seconds_remaining(&a, 0.0), 30);
            assert!((period_progress(&a, 25.0) - (5.0 / 30.0)).abs() < 1e-9);
            assert_eq!(seconds_remaining(&a, 25.0), 5);
        }
    }

    #[test]
    fn secrets_decode_leniently_and_codes_display_grouped() {
        {
            for (encoded, raw) in [
                ("MY", &b"f"[..]),
                ("MZXQ", b"fo"),
                ("MZXW6", b"foo"),
                ("MZXW6YQ", b"foob"),
                ("MZXW6YTB", b"fooba"),
                ("MZXW6YTBOI", b"foobar"),
                ("mzxw 6ytb oi======", b"foobar"),
                ("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", b"12345678901234567890"),
                ("MZ", b"f"),
                ("MZXW6YTBO", b"fooba"),
                ("MZXW6YTBOIA", b"foobar"),
                ("MZXW6YTBOIAAAA", b"foobar\0\0"),
            ] {
                assert_eq!(decode_secret(encoded).as_deref(), Some(raw), "{encoded}");
            }
            for invalid in ["", "MZXW6YTB1", "MZXW6YT!", "MZXW6YTBO!", "A"] {
                let decoded = decode_secret(invalid);
                assert!(
                    decoded.as_ref().is_none_or(Vec::is_empty),
                    "{invalid}: {decoded:?}"
                );
            }
        }

        {
            let a = acc("not valid base32 !!!", Algorithm::Sha1, 6, 30);
            assert_eq!(code_at(&a, 0.0), "••••••");
        }

        {
            assert_eq!(
                group_code("123456")
                    .chars()
                    .filter(char::is_ascii_digit)
                    .count(),
                6
            );
            assert!(group_code("123456").contains('\u{2009}'));
            assert!(group_code("12345678").contains('\u{2009}'));
        }
    }
}
