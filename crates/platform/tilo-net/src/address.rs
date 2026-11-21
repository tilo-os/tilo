//! One judge of whether an address is on the public internet, for every
//! caller that keeps a request off this machine and its network: the fetch
//! proxy ([`crate::proxy`]) and the guard of a page's fetch both ask it, and
//! neither keeps a list of its own.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `address` is on the public internet: not loopback, private
/// (RFC 1918, unique-local), link-local, shared (carrier-grade NAT),
/// multicast, unspecified, documentation or otherwise reserved, and not the
/// address a cloud answers its own machines on. An IPv6 address that carries
/// an IPv4 one is judged by it where it is mapped or translated under the
/// well-known prefix; the blocks that carry or stand for one in another way
/// are not public at all.
#[must_use]
pub fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => public_v4(address),
        IpAddr::V6(address) => public_v6(address),
    }
}

fn public_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    let reserved = a == 0
        || a == 10
        || a == 127
        // Shared address space (carrier-grade NAT), 100.64.0.0/10.
        || (a == 100 && (64..128).contains(&b))
        // Link-local, 169.254.0.0/16, a cloud's metadata among it.
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 168)
        // IETF protocol assignments, 192.0.0.0/24, and documentation.
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        // Benchmarking, 198.18.0.0/15.
        || (a == 198 && (b == 18 || b == 19))
        // Multicast, reserved and broadcast, 224.0.0.0/3.
        || a >= 224
        // The one address Azure answers its own machines on (their metadata
        // and their agent's own service): public by its range, reached only
        // from inside a machine there.
        || address == Ipv4Addr::new(168, 63, 129, 16);
    !reserved
}

fn public_v6(address: Ipv6Addr) -> bool {
    if let Some(v4) = address.to_ipv4_mapped() {
        return public_v4(v4);
    }
    let [s0, s1, s2, s3, s4, s5, s6, s7] = address.segments();
    // NAT64 under the well-known prefix (64:ff9b::/96) reaches the IPv4
    // address it ends in.
    if s0 == 0x0064 && s1 == 0xff9b && s2 == 0 && s3 == 0 && s4 == 0 && s5 == 0 {
        let [a, b] = s6.to_be_bytes();
        let [c, d] = s7.to_be_bytes();
        return public_v4(Ipv4Addr::new(a, b, c, d));
    }
    let reserved =
        // ::, ::1 and the deprecated IPv4-compatible addresses, ::/96.
        (s0 == 0 && s1 == 0 && s2 == 0 && s3 == 0 && s4 == 0 && s5 == 0)
        // Blocks that carry or stand for an IPv4 address in a way this does
        // not judge, none of them a place of the public web: the rest of
        // 64:ff9b::/32 with the local-use translation prefix
        // (64:ff9b:1::/48), 6to4 (2002::/16) and the protocol assignments
        // with Teredo among them (2001::/23).
        || (s0 == 0x0064 && s1 == 0xff9b)
        || s0 == 0x2002
        || (s0 == 0x2001 && (s1 & 0xfe00) == 0)
        // Discard-only, 100::/64.
        || (s0 == 0x0100 && s1 == 0 && s2 == 0 && s3 == 0)
        // Documentation, 2001:db8::/32.
        || (s0 == 0x2001 && s1 == 0x0db8)
        // Unique-local, link-local, site-local and multicast.
        || (s0 & 0xfe00) == 0xfc00
        || (s0 & 0xffc0) == 0xfe80
        || (s0 & 0xffc0) == 0xfec0
        || (s0 & 0xff00) == 0xff00;
    // Only global unicast (2000::/3) is routed on the internet.
    !reserved && (s0 & 0xe000) == 0x2000
}

/// The address `url` writes out where a name would stand, if it writes one:
/// `https://10.0.0.7/` and `https://[::1]/` do, `https://example.com/` does
/// not. Such an address is never looked up, so whoever keeps a request to the
/// public internet judges it ([`is_public`]) before anything is sent; a name
/// is judged where it is looked up.
#[must_use]
pub fn written_out(url: &url::Url) -> Option<IpAddr> {
    let url::Origin::Tuple(_, named, _) = url.origin() else {
        return None;
    };
    literal(&named.to_string())
}

/// `name` as the address it writes out, in brackets or not.
fn literal(name: &str) -> Option<IpAddr> {
    name.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(name)
        .parse()
        .ok()
}

/// Resolves a name to its addresses: the shape of a client's lookup
/// ([`crate::Lookup`]) and of the fetch proxy's, so one value serves them all.
#[cfg(not(target_family = "wasm"))]
pub type Lookup = std::sync::Arc<
    dyn Fn(String) -> std::pin::Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>
        + Send
        + Sync,
>;

/// Why a name is not connected to: an address it answered with is not on the
/// public internet.
#[cfg(not(target_family = "wasm"))]
#[derive(Debug)]
pub struct NotPublic {
    /// The name that was looked up.
    pub name: String,
}

#[cfg(not(target_family = "wasm"))]
impl std::fmt::Display for NotPublic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} answers with an address of this machine or its network",
            self.name
        )
    }
}

#[cfg(not(target_family = "wasm"))]
impl std::error::Error for NotPublic {}

/// The addresses to connect to for `name`: one look-up, every address it
/// answered with judged ([`is_public`]), and all of them handed back or none.
/// A caller connects to exactly these and looks nothing up again, so no
/// second look-up, which might answer otherwise, lies between the judging and
/// the connection. A name that answers with a public and an own address is
/// refused whole, since a connection might take either. A name that is itself
/// an address is judged as that address and is never looked up.
///
/// # Errors
/// The look-up's own failure; `NotFound` for a name that answers with
/// nothing; and for a name that is refused an error that holds
/// [`NotPublic`], which [`not_public`] tells apart.
#[cfg(not(target_family = "wasm"))]
pub async fn public_addresses(lookup: &Lookup, name: &str) -> std::io::Result<Vec<IpAddr>> {
    let addresses = match literal(name) {
        Some(address) => vec![address],
        None => lookup(name.to_owned()).await?,
    };
    if addresses.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{name} does not resolve"),
        ));
    }
    if addresses.iter().any(|address| !is_public(*address)) {
        return Err(std::io::Error::other(NotPublic {
            name: name.to_owned(),
        }));
    }
    Ok(addresses)
}

/// `lookup`, answering a name only as [`public_addresses`] does: the lookup
/// of a connection that reaches the public internet and nothing else.
#[cfg(not(target_family = "wasm"))]
#[must_use]
pub fn public_only(lookup: Lookup) -> Lookup {
    std::sync::Arc::new(move |name: String| {
        let lookup = std::sync::Arc::clone(&lookup);
        Box::pin(async move { public_addresses(&lookup, &name).await })
    })
}

/// The system's resolver, as a [`Lookup`]. A name that is itself an address
/// is answered with that address at once: the resolver is not asked for it.
#[cfg(not(target_family = "wasm"))]
#[must_use]
pub fn system_lookup() -> Lookup {
    std::sync::Arc::new(|name: String| {
        Box::pin(async move {
            // An address is its own answer: nothing is asked for it and
            // nothing is waited for.
            if let Some(address) = literal(&name) {
                return Ok(vec![address]);
            }
            tokio::task::spawn_blocking(move || {
                std::net::ToSocketAddrs::to_socket_addrs(&(name.as_str(), 0))
                    .map(|found| found.map(|address| address.ip()).collect::<Vec<IpAddr>>())
            })
            .await
            .map_err(std::io::Error::other)?
        })
    })
}

/// Whether `lookup` would have `address` dialled, for an address that a URL
/// writes out ([`written_out`]): no connection looks such an address up, so
/// whoever gave its client a lookup asks it here, with the address as the
/// name, before anything is sent. The lookup is asked once and not waited
/// for: a name that is itself an address needs no look-up, and every lookup
/// of this module answers it at once. So a caller on a thread that awaits
/// nothing can ask.
///
/// # Errors
/// The lookup's own refusal, of which [`not_public`] tells the judge's; and
/// for a lookup that answers another address, or would wait, a refusal that
/// says so.
#[cfg(not(target_family = "wasm"))]
pub fn asked_as_written(lookup: &Lookup, address: IpAddr) -> std::io::Result<()> {
    let mut asking = lookup(address.to_string());
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match asking.as_mut().poll(&mut context) {
        std::task::Poll::Ready(Ok(answered)) if answered.contains(&address) => Ok(()),
        std::task::Poll::Ready(Err(refused)) => Err(refused),
        std::task::Poll::Ready(Ok(_)) | std::task::Poll::Pending => Err(std::io::Error::other(
            format!("{address} is not an address its lookup answers"),
        )),
    }
}

/// Whether `error` is the refusal of [`public_addresses`], not a look-up
/// that failed.
#[cfg(not(target_family = "wasm"))]
#[must_use]
pub fn not_public(error: &std::io::Error) -> bool {
    matches!(error.get_ref(), Some(inner) if inner.is::<NotPublic>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An address is its own answer, at once: the system's resolver is not
    /// waited for, and the judge over it refuses an own address and passes a
    /// public one. So the address a URL writes out is asked about from a
    /// thread that awaits nothing. A lookup that would wait for it, or that
    /// answers another address, has it dialled by nobody.
    #[test]
    fn an_address_a_url_writes_out_is_asked_of_a_lookup_without_waiting() {
        let own = IpAddr::from([10, 0, 0, 8]);
        let public = IpAddr::from([93, 184, 216, 34]);
        assert!(asked_as_written(&system_lookup(), own).is_ok());
        let judged = public_only(system_lookup());
        assert!(asked_as_written(&judged, public).is_ok());
        let refused = asked_as_written(&judged, own).expect_err("an own address");
        assert!(not_public(&refused), "the judge's refusal: {refused}");
        let loopback = asked_as_written(&judged, IpAddr::from(Ipv6Addr::LOCALHOST));
        assert!(loopback.is_err_and(|refused| not_public(&refused)));
        let waits: Lookup = std::sync::Arc::new(|_: String| {
            Box::pin(std::future::pending::<std::io::Result<Vec<IpAddr>>>())
        });
        assert!(asked_as_written(&waits, public).is_err());
        let elsewhere: Lookup =
            std::sync::Arc::new(move |_: String| Box::pin(async move { Ok(vec![own]) }));
        assert!(asked_as_written(&elsewhere, public).is_err());
    }

    /// Every form of an address of this machine or its network is not
    /// public, in IPv6 as in IPv4, and the public web is: the one table the
    /// fetch proxy and a page's fetch are both judged by.
    #[test]
    fn every_form_of_an_own_address_is_not_public() {
        let own = [
            // Unspecified, loopback, private, shared, link-local, reserved.
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.7",
            "127.0.0.1",
            "100.64.0.1",
            "100.100.100.200",
            "169.254.169.254",
            "169.254.170.2",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "192.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            // The one address Azure answers its own machines on.
            "168.63.129.16",
            "::",
            "::1",
            "fe80::1",
            "febf::1",
            "fe80::a9fe:a9fe",
            "fc00::1",
            "fd00:ec2::254",
            "fdff:ffff::1",
            "fec0::1",
            "ff02::1",
            // An IPv4 address inside an IPv6 one: mapped, compatible,
            // translated under both prefixes, 6to4 and Teredo.
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.7",
            "::127.0.0.1",
            "::10.0.0.7",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::7f00:1",
            "64:ff9b:1::101:101",
            "2002:7f00:1::",
            "2002:a00:7::1",
            "2002:101:101::",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001::1",
            "2001:1::1",
            // Where no page is: discard-only, documentation, and what lies
            // outside global unicast.
            "100::1",
            "2001:db8::1",
            "4000::1",
        ];
        let let_through: Vec<&str> = own
            .into_iter()
            .filter(|address| is_public(address.parse().unwrap()))
            .collect();
        assert!(
            let_through.is_empty(),
            "addresses of this machine or its network judged public: {let_through:#?}"
        );
        for public in [
            "1.1.1.1",
            "93.184.216.34",
            "172.32.0.1",
            "100.128.0.1",
            "168.63.129.17",
            "2606:4700:4700::1111",
            "2a00:1450:4001:81b::200e",
            "2001:4860:4860::8888",
            "::ffff:1.1.1.1",
            "64:ff9b::5db8:d822",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public} is public");
        }
    }

    /// A name is connected to only where every address it answered with is
    /// public: one look-up, all of its addresses handed back or none, and a
    /// refusal told apart from a look-up that failed.
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn a_name_is_connected_to_only_where_every_address_it_answers_with_is_public() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let asked = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&asked);
        let lookup: Lookup = Arc::new(
            move |name: String| -> std::pin::Pin<
                Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>,
            > {
                counted.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    let answer = |addresses: &[&str]| -> Vec<IpAddr> {
                        addresses
                            .iter()
                            .map(|address| address.parse().unwrap())
                            .collect()
                    };
                    match name.as_str() {
                        "public.example" => Ok(answer(&["93.184.216.34", "2606:2800:220:1::1"])),
                        "both.example" => Ok(answer(&["93.184.216.34", "10.0.0.7"])),
                        "own.example" => Ok(answer(&["::ffff:127.0.0.1"])),
                        "nothing.example" => Ok(Vec::new()),
                        _ => Err(std::io::Error::other("the resolver is down")),
                    }
                })
            },
        );

        let public = public_addresses(&lookup, "public.example").await.unwrap();
        assert_eq!(public.len(), 2, "every address of the one answer");
        assert_eq!(asked.load(Ordering::SeqCst), 1, "one look-up");
        for name in ["both.example", "own.example"] {
            let refused = public_addresses(&lookup, name).await.unwrap_err();
            assert!(not_public(&refused), "{name}: {refused}");
        }
        let nothing = public_addresses(&lookup, "nothing.example")
            .await
            .unwrap_err();
        assert_eq!(nothing.kind(), std::io::ErrorKind::NotFound);
        assert!(!not_public(&nothing));
        let failed = public_addresses(&lookup, "down.example").await.unwrap_err();
        assert!(!not_public(&failed), "a look-up that failed is no refusal");

        // The lookup a connection is given answers the same.
        let only = public_only(Arc::clone(&lookup));
        assert_eq!(only("public.example".to_owned()).await.unwrap(), public);
        let refused = only("both.example".to_owned()).await.unwrap_err();
        assert!(not_public(&refused), "{refused}");
        assert_eq!(asked.load(Ordering::SeqCst), 7, "one look-up a call");

        // A name that is itself an address is judged as that address.
        let written = public_addresses(&lookup, "93.184.216.34").await.unwrap();
        assert_eq!(written, ["93.184.216.34".parse::<IpAddr>().unwrap()]);
        for own in ["10.0.0.5", "[::1]", "::ffff:127.0.0.1"] {
            let refused = public_addresses(&lookup, own).await.unwrap_err();
            assert!(not_public(&refused), "{own}: {refused}");
        }
        assert_eq!(
            asked.load(Ordering::SeqCst),
            7,
            "an address written out is never looked up"
        );
    }

    /// An address written into a URL is found in the forms a URL takes, and
    /// a name is not one.
    #[test]
    fn an_address_a_url_writes_out_is_found_and_a_name_is_not() {
        for (url, address) in [
            ("https://10.0.0.7/x", "10.0.0.7"),
            ("https://[::1]:8443/x", "::1"),
            ("http://[::ffff:127.0.0.1]/", "::ffff:127.0.0.1"),
            ("https://0x7f.1/", "127.0.0.1"),
            ("https://2130706433/", "127.0.0.1"),
        ] {
            assert_eq!(
                written_out(&url::Url::parse(url).unwrap()),
                Some(address.parse().unwrap()),
                "{url}"
            );
        }
        for url in ["https://example.com/", "mailto:ops@example.com"] {
            assert_eq!(written_out(&url::Url::parse(url).unwrap()), None, "{url}");
        }
    }

    /// The system's resolver answers for this machine's own name, and that
    /// name is refused: nothing leaves the machine for it.
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn the_systems_resolver_answers_and_this_machines_own_name_is_refused() {
        let system = system_lookup();
        let found = system("localhost".to_owned()).await.unwrap();
        assert!(!found.is_empty() && found.iter().all(IpAddr::is_loopback));
        let refused = public_addresses(&system, "localhost").await.unwrap_err();
        assert!(not_public(&refused), "{refused}");
    }
}
