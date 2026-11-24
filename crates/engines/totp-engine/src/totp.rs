//! One-time codes and what a setup link says: TOTP (RFC 6238) on the RustCrypto HMAC and SHA
//! primitives, base32 secrets decoded by `data-encoding`, and an `otpauth://` link read into the
//! account it names. No clock of its own: the instant comes from the window that asks.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha512};
use zeroize::Zeroizing;

/// Unused bits in the last symbol are ignored: authenticator apps accept such secrets.
const SECRET_BASE32: data_encoding::Encoding = data_encoding_macro::new_encoding! {
    symbols: "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567",
    check_trailing_bits: false,
};

/// The hash a code is made with, as an `otpauth://` link names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Algorithm {
    #[default]
    Sha1,
    Sha256,
    Sha512,
}

impl Algorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sha1 => "SHA1",
            Self::Sha256 => "SHA256",
            Self::Sha512 => "SHA512",
        }
    }

    /// Unknown values fall back to SHA1.
    pub fn parse(word: &str) -> Self {
        match word.trim().to_ascii_uppercase().as_str() {
            "SHA256" => Self::Sha256,
            "SHA512" => Self::Sha512,
            _ => Self::Sha1,
        }
    }
}

/// The secret in its canonical form: upper-case base32 without spaces or padding.
pub fn normalise_secret(secret: &str) -> String {
    secret
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '=')
        .collect::<String>()
        .to_ascii_uppercase()
}

/// The bytes a base32 secret holds, or none when it is no base32 at all. A final symbol holding
/// no whole byte is dropped rather than failing: authenticator apps accept such secrets.
pub fn decode_secret(secret: &str) -> Option<Vec<u8>> {
    let normalised = normalise_secret(secret);
    if normalised.is_empty() {
        return None;
    }
    decode_base32_lenient(&SECRET_BASE32, normalised.as_bytes())
}

fn decode_base32_lenient(
    encoding: &data_encoding::Encoding,
    symbols: &[u8],
) -> Option<Vec<u8>> {
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

/// The counter a code at `unix_secs` stands on, and how far it is into its step.
pub fn step_at(unix_secs: f64, period: u32) -> (u64, f64) {
    let period = f64::from(period.max(1));
    let step = (unix_secs / period).floor().max(0.0);
    let left = period - (unix_secs - step * period);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a step of a positive instant fits u64 for every instant a clock can name"
    )]
    let counter = step as u64;
    (counter, left)
}

/// The code a secret makes at `unix_secs`, and how long it lasts. `None` when the secret is not
/// base32 at all.
pub fn code_at(
    secret: &str,
    algorithm: Algorithm,
    digits: u8,
    period: u32,
    unix_secs: f64,
) -> Option<Code> {
    let digits = u32::from(digits).clamp(4, 10);
    let key = Zeroizing::new(decode_secret(secret)?);
    if key.is_empty() {
        return None;
    }
    let (counter, left) = step_at(unix_secs, period);
    let value = hotp(&key, counter, digits, algorithm)?;
    Some(Code {
        code: format!("{value:0width$}", width = digits as usize),
        left,
    })
}

/// One code: the digits, and the seconds it still lasts.
#[derive(Clone, Debug, PartialEq)]
pub struct Code {
    pub code: String,
    pub left: f64,
}

/// RFC 4226 §5: the HMAC of the counter, its low nibble picking a four-byte window whose top
/// bit is masked off, then the decimal digits. `None` only when the HMAC setup fails, which it
/// never does (any key length is valid).
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

/// The code as the list plays it back: grouped in threes, as a person reads it out.
pub fn grouped(code: &str) -> String {
    let mut out = String::with_capacity(code.len() + code.len() / 3);
    for (at, one) in code.chars().enumerate() {
        if at > 0 && at % 3 == 0 {
            out.push(' ');
        }
        out.push(one);
    }
    out
}

/// What an `otpauth://` link says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Setup {
    pub issuer: String,
    pub label: String,
    pub secret: String,
    pub algorithm: Algorithm,
    pub digits: u8,
    pub period: u32,
}

/// Reads a setup link into the account it names.
///
/// # Errors
///
/// It is not an `otpauth://` totp link, or it names no secret.
pub fn parse_otpauth_uri(uri: &str) -> Result<Setup, String> {
    let uri = uri.trim();
    let rest = uri
        .strip_prefix("otpauth://")
        .ok_or_else(|| "This is not a setup link".to_string())?;
    let (otp_type, after_type) = match rest.split_once('/') {
        Some((t, r)) => (t.to_ascii_lowercase(), r),
        None => (rest.to_ascii_lowercase(), ""),
    };
    if otp_type == "hotp" {
        return Err(
            "Tilo only makes time-based codes, and this link is for a counter-based one"
                .to_string(),
        );
    }
    if otp_type != "totp" {
        return Err("This link is for a kind of code Tilo does not make".to_string());
    }
    let (label_raw, query_raw) = match after_type.split_once('?') {
        Some((l, q)) => (l, q),
        None => (after_type, ""),
    };
    // The label is a URI path segment, so a `+` is a literal plus: reading it as a space
    // renamed `Google+` to `Google `.
    let label = percent_decode(label_raw);
    let mut secret = String::new();
    let mut issuer_param = String::new();
    let mut algorithm = Algorithm::Sha1;
    let mut digits: u8 = 6;
    let mut period: u32 = 30;
    for pair in query_raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let word = percent_decode(value);
        match key.to_ascii_lowercase().as_str() {
            "secret" => secret = normalise_secret(&word),
            "issuer" => issuer_param = word,
            "algorithm" => algorithm = Algorithm::parse(&word),
            "digits" => digits = word.parse().unwrap_or(6).clamp(4, 10),
            "period" => period = word.parse().unwrap_or(30).clamp(5, 300),
            _ => {}
        }
    }
    if secret.is_empty() {
        return Err("This setup link has no key in it".to_string());
    }
    let (label_issuer, account) = match label.split_once(':') {
        Some((issuer, account)) => (issuer.trim().to_string(), account.trim().to_string()),
        None => (String::new(), label.trim().to_string()),
    };
    let issuer = if issuer_param.trim().is_empty() {
        label_issuer
    } else {
        issuer_param.trim().to_string()
    };
    Ok(Setup {
        issuer,
        label: account,
        secret,
        algorithm,
        digits,
        period,
    })
}

/// A URI component's escapes read back as their bytes.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0usize;
    while let Some(&one) = bytes.get(at) {
        if one == b'%' {
            let hex: Option<Vec<u8>> = bytes
                .get(at + 1..at + 3)
                .and_then(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
                .map(|byte| vec![byte]);
            if let Some(byte) = hex {
                out.extend(byte);
                at += 3;
                continue;
            }
        }
        out.push(one);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn codes_follow_rfc_6238_and_links_read_into_accounts;

        phase a_code_matches_the_rfc_test_vectors_and_last_until_its_step_ends;
        {
            // RFC 6238's SHA-1 vectors, whose secret is "12345678901234567890" in ASCII.
            let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
            assert_eq!(code_at(secret, Algorithm::Sha1, 8, 30, 59.0).unwrap().code, "94287082");
            assert_eq!(code_at(secret, Algorithm::Sha1, 8, 30, 1111111109.0).unwrap().code, "07081804");
            assert_eq!(code_at(secret, Algorithm::Sha1, 8, 30, 1234567890.0).unwrap().code, "89005924");
            let at = code_at(secret, Algorithm::Sha1, 6, 30, 59.0).unwrap();
            assert_eq!(at.code.len(), 6);
            assert!((at.left - 1.0).abs() < 1e-9, "one second left of its step");
            assert_eq!(step_at(59.0, 30).0, 1);
        }

        phase a_setup_link_names_its_service_account_and_key;
        {
            let setup = parse_otpauth_uri(
                "otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&issuer=Acme&algorithm=SHA256&digits=8&period=45",
            )
            .unwrap();
            assert_eq!(setup.issuer, "Acme");
            assert_eq!(setup.label, "bob");
            assert_eq!(setup.secret, "JBSWY3DPEHPK3PXP");
            assert_eq!(setup.algorithm, Algorithm::Sha256);
            assert_eq!(setup.digits, 8);
            assert_eq!(setup.period, 45);
            let plain = parse_otpauth_uri("otpauth://totp/ada%40example.com?secret=jbsw y3dp").unwrap();
            assert_eq!(plain.label, "ada@example.com");
            assert_eq!(plain.secret, "JBSWY3DP", "spaces and case normalise");
            assert_eq!(plain.issuer, "");
            assert_eq!(plain.digits, 6);
            assert!(parse_otpauth_uri("otpauth://hotp/a?secret=AA").is_err());
            assert!(parse_otpauth_uri("https://example.com").is_err());
            assert!(parse_otpauth_uri("otpauth://totp/a?issuer=x").is_err());
        }

        phase a_secret_is_base32_and_codes_are_grouped_for_reading;
        {
            assert!(decode_secret("jbsw y3dp ehpk 3pxp").is_some());
            assert!(decode_secret("not base32!").is_none());
            assert_eq!(grouped("123456"), "123 456");
            assert_eq!(grouped("12345678"), "123 456 78");
        }
    }
}
