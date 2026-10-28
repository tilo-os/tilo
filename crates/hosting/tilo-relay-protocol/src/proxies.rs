//! Address ranges whose forwarded headers a host believes: the ingress or
//! proxy in front of it, never the open internet. Both the room service
//! (whose `X-Forwarded-For` names a visitor) and a relay (whose proxy names
//! a client's country) read them the same way.

use std::net::IpAddr;

/// Address ranges, each an address and a prefix length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<(IpAddr, u8)>);

impl std::str::FromStr for TrustedProxies {
    type Err = String;

    /// Comma-separated `address[/prefix]` ranges; empty for none.
    fn from_str(text: &str) -> Result<Self, String> {
        let mut ranges = Vec::new();
        for item in text
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            let (address, bits) = match item.split_once('/') {
                Some((address, bits)) => (address, Some(bits)),
                None => (item, None),
            };
            let address: IpAddr = address
                .parse()
                .map_err(|_| format!("{item} is not an address"))?;
            let max = if address.is_ipv4() { 32 } else { 128 };
            let bits = bits
                .map_or(Ok(max), str::parse::<u8>)
                .map_err(|_| format!("{item} has an invalid prefix"))?;
            if bits > max {
                return Err(format!("{item} has a prefix longer than its address"));
            }
            ranges.push((address, bits));
        }
        Ok(Self(ranges))
    }
}

impl std::fmt::Display for TrustedProxies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for (address, bits) in &self.0 {
            if !first {
                f.write_str(",")?;
            }
            first = false;
            write!(f, "{address}/{bits}")?;
        }
        Ok(())
    }
}

/// The address as the high bits of a `u128`, so IPv4 and IPv6 prefixes
/// mask the same way.
fn prefix_bits(ip: IpAddr) -> u128 {
    match ip {
        IpAddr::V4(v4) => u128::from(u32::from(v4)) << 96,
        IpAddr::V6(v6) => u128::from(v6),
    }
}

impl TrustedProxies {
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|(range, bits)| {
            if range.is_ipv4() != ip.is_ipv4() {
                return false;
            }
            // An IPv4 address sits in the top 32 bits, so its prefix of
            // `bits` keeps the top `bits` bits just like an IPv6 one.
            let mask = u128::MAX
                .checked_shl(128_u32.saturating_sub(u32::from(*bits)))
                .unwrap_or(0);
            prefix_bits(*range) & mask == prefix_bits(ip) & mask
        })
    }

    /// Whether no range is listed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_parse_print_and_match_by_prefix() {
        let trusted: TrustedProxies = "10.42.0.0/16, ::1".parse().unwrap();
        assert!(trusted.contains("10.42.7.1".parse().unwrap()));
        assert!(!trusted.contains("10.43.0.1".parse().unwrap()));
        assert!(trusted.contains("::1".parse().unwrap()));
        assert_eq!(trusted.to_string(), "10.42.0.0/16,::1/128");
        assert_eq!(trusted.to_string().parse::<TrustedProxies>(), Ok(trusted));
        assert!("10.0.0.0/33".parse::<TrustedProxies>().is_err());
        assert!("".parse::<TrustedProxies>().unwrap().is_empty());
    }
}
