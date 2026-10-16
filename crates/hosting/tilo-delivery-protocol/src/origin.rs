//! The bare `https://host[:port]` origin that auth bindings, descriptors and
//! join locators name.

const MAX_ORIGIN_BYTES: usize = 2048;

/// Whether `origin` is `https://` followed by a lowercase DNS name or a
/// bracketed IPv6 address and an optional nonzero port — no path, query,
/// fragment or userinfo.
pub(crate) fn is_https_origin(origin: &str) -> bool {
    https_origin(origin).is_some()
}

/// Deserializes an origin, refusing anything [`is_https_origin`] rejects.
pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = <&str as serde::Deserialize>::deserialize(d)?;
    if !is_https_origin(value) {
        return Err(serde::de::Error::custom("invalid https origin"));
    }
    Ok(value.to_owned())
}

fn https_origin(origin: &str) -> Option<()> {
    if origin.len() > MAX_ORIGIN_BYTES || !origin.is_ascii() {
        return None;
    }
    let authority = origin.strip_prefix("https://")?;
    if authority.is_empty()
        || authority.bytes().any(|b| {
            b.is_ascii_whitespace()
                || b.is_ascii_control()
                || matches!(b, b'/' | b'\\' | b'?' | b'#' | b'@')
        })
    {
        return None;
    }
    let (host, port) = if let Some(ipv6) = authority.strip_prefix('[') {
        let (host, suffix) = ipv6.split_once(']')?;
        host.parse::<std::net::Ipv6Addr>().ok()?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':')?)
        };
        (None, port)
    } else {
        let (host, port) = authority
            .rsplit_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)));
        (Some(host), port)
    };
    if let Some(host) = host
        && (host.len() > 253
            || host.split('.').any(|part| {
                part.is_empty()
                    || part.len() > 63
                    || part.starts_with('-')
                    || part.ends_with('-')
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            }))
    {
        return None;
    }
    if let Some(port) = port
        && (port.is_empty()
            || !port.bytes().all(|b| b.is_ascii_digit())
            || port.parse::<u16>().ok().is_none_or(|p| p == 0))
    {
        return None;
    }
    Some(())
}
