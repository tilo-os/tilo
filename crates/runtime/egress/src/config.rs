//! Host configuration shared by portable policy clients and Linux enforcement.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

/// The resolvers named in a `resolv.conf`, on port 53.
pub fn nameservers(resolv_conf: &str) -> Vec<SocketAddr> {
    resolv_conf
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .filter_map(|rest| {
            let addr = rest.trim();
            // A scoped link-local resolver (`fe80::1%eth0`) can't be named
            // by a SocketAddr; it is skipped.
            addr.parse::<IpAddr>()
                .ok()
                .map(|ip| SocketAddr::new(ip, 53))
        })
        .collect()
}

/// Find the pasta binary without starting it.
pub fn find_pasta() -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|d| d.join("pasta"))
                .find(|p| p.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("/usr/bin/pasta"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nameservers_come_from_resolv_conf() {
        let conf = "# generated\nnameserver 10.0.0.1\nnameserver  2001:db8::53\nnameserver fe80::1%eth0\nsearch lan\n";
        assert_eq!(
            nameservers(conf),
            vec![
                "10.0.0.1:53".parse::<SocketAddr>().unwrap(),
                "[2001:db8::53]:53".parse().unwrap()
            ]
        );
    }
}
