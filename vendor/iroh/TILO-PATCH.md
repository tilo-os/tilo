# Tilo patch: an idle endpoint wakes for nothing

crates.io `iroh` 1.0.3, verbatim (archive SHA-256
`460de6bc52163b41b1646931f2897e5ab986f0966ade444467fec25024751a72`, upstream
commit `f2eb930dda3779c6d852b72f3712aacd6e573ab1`) but for the changes
below, each marked `TILO PATCH` in the source, and its `[lib]` building an
`rlib` only. iroh 1.3.0, the newest release
when this was written, has the same code in every place patched. Pinned by
`crates/system/fs/tests/iroh_idle.rs` and
`crates/platform/tilo-net/tests/iroh_trust.rs`.

Tilo binds every endpoint with an empty relay map until someone could need to
dial the device, and empties it again five minutes after the last
collaborator leaves (`crates/system/fs/src/reachability.rs`,
tilo-fs's `bind_endpoint_with_key`). With no relays a net report is skipped
(`DirectAddrUpdateState::run`), yet the endpoint kept waking.

## The re-STUN tick waits for a relay (`src/socket.rs`)

The socket actor re-ran its address discovery every 20-26 s
(`periodic_re_stun_timer`) whether or not there was a relay to ask. With an
empty map the run only republishes "no report", so the tick woke the runtime
for nothing. Its `select!` arm is now disabled while the relay map is empty,
and `handle_relay_map_change` starts the timer afresh, so the stale deadline
of an unpolled timer does not fire beside the run a new relay starts. With
relays the cadence is the one described under "The net report rests".

That tick also hid a gap: with no relays, `re_stun` sets the watched net
report to `(None, reason)`, and `Watchable::set` notifies nobody when the
value equals the last one. A second link change for the same reason (an
address added after another was) therefore did not re-collect the local
addresses until the next periodic tick, up to 26 s later. `re_stun` now
collects them itself while the map is empty.

## No home relay once the relays are gone (`src/socket.rs`, `src/socket/transports*.rs`)

Only a net report demotes a home relay (`RelayActor::on_network_change`), and
none runs without relays, so after `Endpoint::remove_relay` emptied the map
the home relay stayed: exempt from the inactivity close, pinging every 15 s,
and still named in `Endpoint::addr`. `handle_relay_map_change` now sends the
relay actor `ReleaseHomeRelay` when the map is empty, which clears the home
relay and tells every active relay it is no longer home; each then closes on
the existing rule, a minute after it last carried data.

A net report still probing when the map emptied finishes afterwards naming
the relay it probed, and `Actor::handle_net_report_report` made that relay
home again for good, the release being already done; under load that was the
usual order when the relays rested soon after waking. A report's preferred
relay, and the current home relay it falls back to, now count only while
that relay is still in the map.

## The net report rests while it finds nothing new (`src/socket.rs`)

With relays, the socket actor re-ran its net report every 20-26 s for the
life of the endpoint: QUIC address discovery against every relay, DNS, a
port-mapping probe, about a hundred wakeups a run. On an idle share host or
recipient that was 3-5 of its wakeups a second. A periodic report that finds
the same direct addresses and home relay as the one before now makes
the next one wait four times the period, and then five minutes
(`re_stun_delay`, `RE_STUN_REST_MAX`); a report that finds anything new, a
link change and a relay map change go back to the usual cadence. A new
connection while resting runs a report at once (`ActorMessage::AddConnection`),
so holepunching starts from fresh addresses. What only the periodic report
sees, a new public address behind a router with no local link change, is
noticed within five minutes instead of 26 s; a live connection keeps its own
NAT binding with its keep-alives.

## Longer keep-alives are allowed (`src/endpoint/quic.rs`, `src/socket/remote_map/remote_state.rs`)

`QuicTransportConfigBuilder::default_path_keep_alive_interval` ignored any
interval above 5 s and `default_path_max_idle_timeout` clamped any timeout
above 15 s, so every path of every connection pinged every 5 s. An idle
connection held open (tilo-fs's share recipient holds two) cost both ends a
wakeup per path every 5 s. Both now take the value given. A relay path's idle
timeout was set to 30 s whatever the configured default; it now keeps the
configured default when that is longer, so a longer keep-alive does not
time out the relay path first. iroh's defaults are unchanged; tilo-fs sets a
25 s keep-alive and a 60 s idle timeout (`crates/system/fs/src/sync.rs`).

## Built as an rlib only (`Cargo.toml`)

Upstream also builds a `cdylib` (for wasm), and Cargo leaves the hash out of
a path package's library filenames when it builds a dylib or cdylib. Once
the crate is a path dependency, two builds of it in one Cargo run (two
feature sets or profiles, as a full-workspace `cargo nextest run` makes)
write the same `target/debug/deps/libiroh.rlib` and `.so` ("output filename
collision"), and dependents then fail with E0460/E0463. Nothing in Tilo loads
the cdylib, so the `[lib]` builds `rlib` only.

## A browser keeps its relay across a network change (`src/socket/transports/relay/actor.rs`)

After a major network change the relay actor checks each relay connection's
local address against the machine's IPs. A browser's WebSocket has no local
address, and upstream closed such a connection ("No local address"). In a
browser every `online` event or wall-clock jump (a throttled background tab,
a resume) is a major change, so a page lost its healthy home relay and the
datagrams in flight while it dialed again. A connection without a local
address is now pinged instead; a dead one still ends through the ping
timeout. Native connections always have a local address and keep the check.
`test_check_connection_without_local_addr_pings` pins it.

## Relays are trusted through the system store (`src/endpoint.rs`, `src/tls/system_store.rs`, `Cargo.toml`)

An endpoint built without `Builder::ca_tls_config` verified relays (the relay
connection and the net report's HTTPS probe), pkarr servers and
DNS-over-HTTPS resolvers against `webpki_roots`, a compiled-in copy of
Mozilla's roots. Every other Tilo client trusts the system store
(`tilo_net::client_config`): Tilo's bundle at
`/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem` on Linux, read by the
platform verifier through `crates/system/tilo-native-certs`. So a relay
whose packet relay is served under a CA installed in that bundle, as a local
stack's is, failed its own probe with `UnknownIssuer` and never got a home
relay. Unset, the trust is now that store: the platform verifier
(`rustls-platform-verifier`, the one tilo-net uses), and on Android, where
it needs a JVM context, Android's system roots through
`rustls-native-certs`, as `tilo_net::client_config` does there. A store that
cannot be read leaves the endpoint bound with a verifier that refuses every
server, so direct connections still work and no relay is trusted. In a
browser the browser does the TLS, and the default is unchanged.
`tilo-net`'s `iroh_trust` tests pin it: a relay under a local CA is
refused at the default, and reached once the CA is in the store.

## Datagrams follow the relay handshake's answer (`src/socket/transports/relay/actor.rs`)

The relay actor builds its relay clients with
`ClientBuilder::send_before_confirmed(true)` (`vendor/iroh-relay`), so the
datagrams queued while it dialled, such as a connection's QUIC Initial, go
out right behind the handshake's answer instead of a round trip later. A
relay that denies the client ends the connection's stream with that error,
and the actor dials again as it does for a lost connection.

## Upstream

Prepared for an upstream issue/PR against `n0-computer/iroh` (not opened):

> **Idle endpoints with an empty relay map still wake every 20-26 s and keep
> their home relay.** With `RelayMode::Disabled`, or after
> `Endpoint::remove_relay` emptied the map, `periodic_re_stun_timer` keeps
> firing although `DirectAddrUpdateState::run` returns early for an empty
> map, and the home relay chosen earlier is never demoted, so its
> connection and 15 s ping last until the endpoint closes. Proposed: disable
> the timer arm while the map is empty and reset the timer on
> `RelayMapChange`; on a change that empties the map, clear the home relay
> and send `SetHomeRelay(false)` to the active relays, and do not make a
> relay home that has left the map (a report in flight during the removal
> still names it). Also: with an empty
> map, a second link change with the same `UpdateReason` sets an equal
> `(None, why)` net report, which `Watchable::set` does not notify, so
> local direct addresses are only refreshed by the periodic tick; collect
> them in `re_stun` when the map is empty.
>
> **Idle endpoints re-run the net report every 20-26 s forever, and paths
> cannot keep alive less often than every 5 s.** Proposed: back the periodic
> net report off while consecutive reports find the same direct addresses and
> home relay (up to a few minutes), returning to the usual cadence on a
> changed report, a link change, a relay map change or a new connection; and
> let `default_path_keep_alive_interval`/`default_path_max_idle_timeout` take
> longer values (keeping a relay path's idle timeout at least the configured
> default), so applications that hold idle connections can trade failover
> speed for fewer wakeups.
>
> **A browser drops its relay connection on every network change.**
> `ActiveRelayActor` answers `CheckConnection` for a stream without a local
> address with `LocalAddrMissing`, and a `wasm_browser` stream never has one.
> Proposed: ping such a connection like one whose address is still local.
