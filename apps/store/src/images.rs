//! Bounded, fail-closed artwork loading for catalogue icons and screenshots.

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use ui::{Fetcher, ImageStore};

pub const MAX_ARTWORK_BYTES: u64 = 8 * 1024 * 1024;

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, d] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip == Ipv4Addr::BROADCAST
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 0 && c == 2)
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 240
                || d == 255 && a == 0)
        }
        IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            let mapped = ip.to_ipv4_mapped();
            mapped.map_or_else(
                || {
                    !(ip == Ipv6Addr::LOCALHOST
                        || ip == Ipv6Addr::UNSPECIFIED
                        || ip.is_multicast()
                        || first & 0xfe00 == 0xfc00
                        || first & 0xffc0 == 0xfe80
                        || first & 0xffc0 == 0xfec0)
                },
                |ipv4| public_ip(IpAddr::V4(ipv4)),
            )
        }
    }
}

fn safe_artwork_url(value: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url
        .host_str()?
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || host
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("local"))
        || host.parse::<std::net::IpAddr>().is_ok()
    {
        return None;
    }
    Some(url)
}

pub fn image_fetcher() -> Fetcher {
    Arc::new(move |value: &str| {
        let url = safe_artwork_url(value)?;
        let host = url.host_str()?.to_string();
        let port = url.port_or_known_default()?;
        let addresses = (host.as_str(), port)
            .to_socket_addrs()
            .ok()?
            .collect::<Vec<_>>();
        if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
            return None;
        }
        // Pin this request to the validated address. This closes the usual
        // validate-then-resolve DNS-rebinding gap while preserving TLS SNI.
        let client = tilo_net::http::blocking_client()
            .user_agent("tilo-store-artwork/1")
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .resolve(&host, *addresses.first()?)
            .build()
            .ok()?;
        let mut response = client.get(url).send().ok()?;
        if !response.status().is_success() || response.status().is_redirection() {
            return None;
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_ARTWORK_BYTES)
        {
            return None;
        }
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(MAX_ARTWORK_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 <= MAX_ARTWORK_BYTES && !bytes.is_empty()).then_some(bytes)
    })
}

pub fn image_store() -> ImageStore {
    ImageStore::with_workers(&image_fetcher(), 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Artwork URLs refuse local networks, ambient credentials and every non-public range.
    #[test]
    fn artwork_urls_refuse_non_public_hosts_and_credentials() {
        // Case `artwork_urls_refuse_local_network_and_ambient_credentials`.
        {
            for url in [
                "http://cdn.example/icon.png",
                "https://user:secret@example.com/icon.png",
                "https://localhost/icon.png",
                "https://127.0.0.1/icon.png",
                "https://[::1]/icon.png",
                "https://printer.local/icon.png",
            ] {
                assert!(safe_artwork_url(url).is_none(), "accepted {url}");
            }
            assert!(safe_artwork_url("https://cdn.example/icon.png").is_some());
        }

        // Case `artwork_address_filter_refuses_every_non_public_range`.
        {
            for ip in [
                "127.0.0.1",
                "10.0.0.1",
                "172.16.0.1",
                "192.168.1.1",
                "169.254.1.1",
                "100.64.0.1",
                "192.0.2.1",
                "198.51.100.1",
                "203.0.113.1",
                "::1",
                "fe80::1",
                "fd00::1",
            ] {
                assert!(!public_ip(ip.parse().unwrap()), "accepted {ip}");
            }
            assert!(public_ip("1.1.1.1".parse().unwrap()));
            assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
        }
    }
}
