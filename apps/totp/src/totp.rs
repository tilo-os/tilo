//! Time-based one-time password generation — RFC 6238 (TOTP) layered on
//! RFC 4226 (HOTP), implemented directly on the audited `RustCrypto`
//! `hmac` + `sha1` + `sha2` primitives, with RFC 4648 base32 secret
//! decoding via `data-encoding`.
//!
//! Why hand-rolled rather than `totp-rs`: the `RustCrypto` crates are
//! already in the workspace lockfile (so no extra fetch), and the RFC
//! 6238 construction is ~30 lines. This keeps the shared fleet build
//! green and gives the UI full control over the algorithm / digit /
//! period surface it exposes in the add form.
//!
//! The single entry points the UI calls are [`current_code`] (the code
//! string for "now") and [`period_progress`] (the 0..1 fraction of the
//! current step still remaining — drives the countdown ring).

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use zeroize::Zeroizing;

use crate::model::{Account, Algorithm};

/// RFC 4648 base32 without padding. Unused bits in the last symbol are
/// ignored rather than required to be zero: authenticator apps accept such
/// secrets, and vaults written by earlier builds may hold them.
const SECRET_BASE32: data_encoding::Encoding = data_encoding_macro::new_encoding! {
    symbols: "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567",
    check_trailing_bits: false,
};

/// Decode a normalised base32 (RFC 4648, no padding required) secret to
/// raw key bytes. Returns `None` when the string isn't valid base32.
pub fn decode_secret(secret: &str) -> Option<Vec<u8>> {
    let normalised = crate::model::normalise_secret(secret);
    if normalised.is_empty() {
        return None;
    }
    decode_base32_lenient(&SECRET_BASE32, normalised.as_bytes())
}

/// Decode `symbols`, tolerating a length no encoder produces (1, 3 or 6
/// symbols past a full block): such a final symbol holds no whole byte, so
/// it is checked to be a symbol and then dropped, as earlier builds did.
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

/// Seconds since the Unix epoch for "now". Centralised so tests and the
/// ring share one clock source.
pub fn unix_now() -> f64 {
    ui::time::Duration::from_nanos(ui::time::unix_now_ns()).as_secs_f64()
}

/// RFC 4226 HOTP value for an 8-byte big-endian counter. Returns the
/// truncated integer already reduced modulo `10^digits`, or `None` if the
/// HMAC construction itself failed — which `new_from_slice` never actually
/// does for HMAC (any key length is valid; the fallible signature only
/// exists because it comes from the shared `Mac` trait), but propagating
/// it as an `Option` beats an `expect()` that can panic on secret-derived
/// input. The modulus is computed in `u64` so digit counts up to 10 can't
/// overflow.
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

    // Dynamic truncation (RFC 4226 §5.3): the low nibble of the last byte
    // selects a 4-byte window; mask the top bit of the first byte to stay
    // positive. `digest` is always at least 20 bytes (the shortest HMAC
    // output, SHA1) and the nibble caps the offset at 15, so the window
    // always exists — `.get()` plus a zeroed fallback documents that
    // instead of indexing straight into secret-derived bytes.
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

/// The TOTP code string for `account` at Unix time `unix_secs`, derived
/// per RFC 6238 (`counter = floor(t / period)`). Returns a
/// zero-padded, `digits`-wide string, or a `digits`-wide run of `•`
/// when the secret can't be decoded (so the row still lays out).
pub fn code_at(account: &Account, unix_secs: f64) -> String {
    let digits = u32::from(account.digits).clamp(4, 10);
    let Some(key) = decode_secret(&account.secret) else {
        return "•".repeat(digits as usize);
    };
    // Hold the raw HMAC key in a buffer that is wiped from the heap once
    // this code has been derived, so long-lived secret bytes don't linger.
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

/// Convenience: the current code for an account.
pub fn current_code(account: &Account) -> String {
    code_at(account, unix_now())
}

/// Why this device's clock may generate codes every server rejects, when it
/// can be known.
///
/// TOTP has no way to ask the server what time it thinks it is; the honest
/// signal is whether this device's clock is synchronised at all. A live
/// Linux session asks systemd's NTP state once; a capture and a test never
/// run a command and report no warning.
pub fn clock_drift_warning() -> Option<&'static str> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(all(not(test), target_os = "linux", not(target_family = "wasm")))]
    {
        if ui::time::wall_frozen() {
            return None;
        }
        let output = std::process::Command::new("timedatectl")
            .args(["show", "-p", "NTPSynchronized", "--value"])
            .output()
            .ok()?;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() != "yes" {
            return Some(
                "This device\u{2019}s clock is not synchronised; codes may be rejected. \
                 Turn on automatic time, then try again.",
            );
        }
        None
    }
    #[cfg(all(not(test), not(all(target_os = "linux", not(target_family = "wasm")))))]
    {
        None
    }
}

/// Group a code for readability: 6 digits → "123 456", 8 → "1234 5678",
/// otherwise split in the middle. Masked placeholder strings pass
/// through grouped the same way.
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

/// Whole seconds remaining in the current step (1..=period).
pub fn seconds_remaining(account: &Account, unix_secs: f64) -> u32 {
    let period = f64::from(account.period.max(1));
    let within = unix_secs.rem_euclid(period);
    (period - within).ceil().max(1.0) as u32
}

/// Fraction of the current period still remaining, in `[0, 1]`. `1.0` at
/// the instant a new code begins, decaying to `0.0` just before the next
/// rotation. Sub-second precise so the countdown ring sweeps smoothly.
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

    /// RFC 6238 vectors, period stability and progress/remaining.
    #[test]
    fn codes_match_rfc6238_and_track_the_clock() {
        // Case `rfc6238_sha1_vectors`.
        // RFC 6238 Appendix B test vectors. The published vectors use an
        // ASCII seed; "12345678901234567890" base32-encodes to
        // "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ". Verify SHA1 at the listed
        // timestamps.
        {
            let a = acc("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", Algorithm::Sha1, 8, 30);
            assert_eq!(code_at(&a, 59.0), "94287082");
            assert_eq!(code_at(&a, 1_111_111_109.0), "07081804");
            assert_eq!(code_at(&a, 1_234_567_890.0), "89005924");
            assert_eq!(code_at(&a, 2_000_000_000.0), "69279037");
        }

        {
            let a = acc("JBSWY3DPEHPK3PXP", Algorithm::Sha1, 6, 30);
            // 100s and 119s both fall in the [90, 120) step (counter 3), so the
            // code is identical.
            assert_eq!(code_at(&a, 100.0), code_at(&a, 119.0));
            // Crossing the 120s boundary into the next step changes the code
            // (overwhelmingly likely).
            assert_ne!(code_at(&a, 100.0), code_at(&a, 130.0));
        }

        {
            let a = acc("JBSWY3DPEHPK3PXP", Algorithm::Sha1, 6, 30);
            // Exactly at a step boundary: full period remaining.
            assert!((period_progress(&a, 0.0) - 1.0).abs() < 1e-9);
            assert_eq!(seconds_remaining(&a, 0.0), 30);
            // 25s into a 30s step: ~1/6 remaining, 5s left.
            assert!((period_progress(&a, 25.0) - (5.0 / 30.0)).abs() < 1e-9);
            assert_eq!(seconds_remaining(&a, 25.0), 5);
        }
    }

    /// RFC 4648 decoding, masked placeholder for invalid secrets, and thin-space grouping.
    #[test]
    fn secrets_decode_leniently_and_codes_display_grouped() {
        // Case `secrets_decode_like_rfc4648_and_stay_lenient`.
        // RFC 4648 section 10 vectors, plus the inputs the lenient decoder
        // must keep accepting: unused trailing bits that are not zero, and a
        // stray final symbol that carries no whole byte.
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
