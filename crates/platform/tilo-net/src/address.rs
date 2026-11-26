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
/// not. Reads the parsed host (`Host::Ipv4`, `Host::Ipv6`, or a non-special
/// scheme's opaque `Host::Domain` whose text parses as an `IpAddr`), and for a
/// URL without a host (such as `blob:http://127.0.0.1/…`) reads the origin
/// tuple's host. Such an address is never looked up, so whoever keeps a
/// request to the public internet judges it ([`is_public`]) before anything is
/// sent; a name is judged where it is looked up.
#[must_use]
pub fn written_out(url: &url::Url) -> Option<IpAddr> {
    match url.host() {
        Some(url::Host::Ipv4(address)) => Some(IpAddr::V4(address)),
        Some(url::Host::Ipv6(address)) => Some(IpAddr::V6(address)),
        Some(url::Host::Domain(text)) => text.parse::<IpAddr>().ok(),
        None => match url.origin() {
            url::Origin::Tuple(_, url::Host::Ipv4(address), _) => Some(IpAddr::V4(address)),
            url::Origin::Tuple(_, url::Host::Ipv6(address), _) => Some(IpAddr::V6(address)),
            url::Origin::Tuple(_, url::Host::Domain(text), _) => text.parse::<IpAddr>().ok(),
            url::Origin::Opaque(_) => None,
        },
    }
}

/// Whether `url` writes out a literal address ([`written_out`]) that is not on
/// the public internet ([`is_public`]). A domain name or a URL that names no
/// address is never one.
#[must_use]
pub fn is_non_public_literal(url: &url::Url) -> bool {
    written_out(url).is_some_and(|address| !is_public(address))
}

/// `name` as the address it writes out, in brackets or not.
#[cfg(not(target_family = "wasm"))]
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

/// Why no webhook delivery is posted to `url`, where none is: over `https` to an address on the
/// public internet, or over plain `http` to this machine's own loopback, for the person's own
/// programs there. A name is judged where the connection looks it up (see `public_only`); this
/// refuses only what the URL itself writes out.
#[must_use]
pub fn webhook_refusal(url: &str) -> Option<&'static str> {
    /// What either says of an address of its own written into a URL.
    const OWN_ADDRESS: &str = "a webhook posts to the public internet, and this URL names an \
                               address of the device's own machine or network";
    /// What a URL that is no such address says.
    const PUBLIC_OR_HERE: &str = "a webhook posts to an https URL on the public internet, or \
                                  over plain http to this computer";
    const LOCALHOST: &str = "localhost";
    let Ok(url) = url::Url::parse(url) else {
        return Some(PUBLIC_OR_HERE);
    };
    let written = written_out(&url);
    match url.scheme() {
        "https" if written.is_some_and(|address| !is_public(address)) => Some(OWN_ADDRESS),
        "https" => None,
        "http" => match written {
            Some(address) if address.is_loopback() => None,
            None if url.domain() == Some(LOCALHOST) => None,
            _ => Some(PUBLIC_OR_HERE),
        },
        _ => Some(PUBLIC_OR_HERE),
    }
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
