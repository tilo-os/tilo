# swarm-discovery provenance

- Package: `swarm-discovery` 0.6.6
- Registry checksum:
  `c349937bc28998d12eed97a4fabcf71e78e15342b10af81eb99e6e0c248ba5fd`
- Upstream revision: `6ab4e590b36efd0135d25a3f2cf5520f074daefd`
- Repository: `https://github.com/rkuhn/swarm-discovery`
- License: Apache-2.0, `LICENSE.Apache_2.0` from the published archive.

Local patch, `src/socket.rs`: 0.6.6 says a failed send on an
interface-specific socket once, and again only when its error changes. The
shared IPv4 and IPv6 sockets, which a discoverer with no interface list sends
on (Tilo's LAN lookup on most machines), still warned on every announcement,
about once a second, wherever multicast is refused, and so did "failed to
send mDNS on any IPv4 interface". The patch keys upstream's failure record by
address, so the shared sockets are tracked under their multicast group's, says
those failures once too, and says the every-interface error only in the round
an interface's failure is new. Everything else is the published source. Only
the library is carried: the crate's own examples, its discovery test and the
dev-dependencies no build here resolves are left out. `src/socket.rs` adds a
unit test beside upstream's two.

Upstream-ready description: "Say a failed send on the shared sockets once,
like the interface-specific ones. Key `SendFailures` by `IpAddr`, track the
shared IPv4 and IPv6 sockets under their multicast group, and log the
every-interface error only when an interface's failure is new."

The workspace root selects this copy with its reviewed Cargo patch.
