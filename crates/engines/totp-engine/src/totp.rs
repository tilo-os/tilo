//! What a setup link says, and whether a key reads: an `otpauth://` link read into the
//! account it names, and base32 secrets decoded by `data-encoding`. The code itself is made
//! where the seed is, in the workspace's vault.

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
        fn links_read_into_accounts_and_secrets_read_as_base32;

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

        phase a_secret_is_base32;
        {
            assert!(decode_secret("jbsw y3dp ehpk 3pxp").is_some());
            assert!(decode_secret("not base32!").is_none());
        }
    }
}
