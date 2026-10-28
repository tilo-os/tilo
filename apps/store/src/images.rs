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

/// Keep this much artwork on disk before the oldest files make room.
const ARTWORK_CACHE_BYTES: u64 = 256 * 1024 * 1024;
/// Check the artwork cache's size after this many new files.
const EVICT_EVERY: usize = 64;

/// The artwork cache under the Store's cache directory.
fn artwork_dir() -> Option<std::path::PathBuf> {
    crate::config::cache_dir().map(|dir| dir.join("artwork"))
}

/// `network` behind a disk cache in `dir`: artwork fetched once is read from
/// disk afterwards, so icons and screenshots a window showed before appear
/// at once, and only new ones wait for the network. Files are named by the
/// BLAKE3 of their URL; the oldest are removed once the cache outgrows
/// `ARTWORK_CACHE_BYTES`.
pub fn cached_fetcher(network: Fetcher, dir: Option<std::path::PathBuf>) -> Fetcher {
    let written = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Arc::new(move |url: &str| {
        let Some(dir) = dir.as_ref() else {
            return network(url);
        };
        let file = dir.join(blake3::hash(url.as_bytes()).to_hex().as_str());
        if let Ok(metadata) = std::fs::metadata(&file)
            && metadata.len() > 0
            && metadata.len() <= MAX_ARTWORK_BYTES
            && let Ok(bytes) = std::fs::read(&file)
        {
            return Some(bytes);
        }
        let bytes = network(url)?;
        if tilo_files::native::atomic::write_creating_parent(&file, &bytes).is_ok()
            && written
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .is_multiple_of(EVICT_EVERY)
        {
            evict_oldest(dir, ARTWORK_CACHE_BYTES);
        }
        Some(bytes)
    })
}

/// Remove the least recently written files in `dir` until it holds at most
/// `budget` bytes.
fn evict_oldest(dir: &std::path::Path, budget: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then(|| {
                (
                    metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                    metadata.len(),
                    entry.path(),
                )
            })
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, size, _)| *size).sum();
    if total <= budget {
        return;
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    for (_, size, path) in files {
        if total <= budget {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

pub fn image_store() -> ImageStore {
    ImageStore::with_workers(&cached_fetcher(image_fetcher(), artwork_dir()), 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artwork_is_fetched_once_and_then_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let network: Fetcher = Arc::new(move |_url: &str| {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(vec![1, 2, 3])
        });
        let fetch = cached_fetcher(network, Some(dir.path().to_path_buf()));
        assert_eq!(fetch("https://cdn.example/icon.png"), Some(vec![1, 2, 3]));
        assert_eq!(fetch("https://cdn.example/icon.png"), Some(vec![1, 2, 3]));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        std::fs::write(dir.path().join("old"), vec![0u8; 64]).unwrap();
        evict_oldest(dir.path(), 4);
        assert!(std::fs::read_dir(dir.path()).unwrap().count() <= 1);
    }

    #[test]
    fn artwork_urls_refuse_non_public_hosts_and_credentials() {
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
