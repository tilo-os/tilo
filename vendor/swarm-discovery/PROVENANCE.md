# swarm-discovery provenance

- Package: `swarm-discovery` 0.6.5
- Registry checksum:
  `d7e56b7e93eaffb9baba3e64936a5912540847516fc5f26d66ad18044b8d4a14`
- Repository: `https://github.com/rkuhn/swarm-discovery`
- License: Apache-2.0, `LICENSE.Apache_2.0` from the published archive.

Local patch, `src/socket.rs`: a multicast send that fails said so on every
announcement, about once a second, wherever multicast is refused — at error
level for IPv4 and warn for IPv6, on each interface, burying the log of the
process that runs discovery. The patch says a failure once, again only when
its cause changes, and says once when sending works again. A failure on every
interface is an error, one on some a warning. Everything else is the
published source. Only the library is carried: the crate's own examples, its
discovery test and the dev-dependencies no build here resolves are left out,
and `src/socket.rs` holds a unit test of what a send says.

The workspace root selects this copy with its reviewed Cargo patch.
