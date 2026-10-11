# Tilo patch: an endpoint without relays wakes for nothing

crates.io `iroh` 1.0.3, verbatim (archive SHA-256
`460de6bc52163b41b1646931f2897e5ab986f0966ade444467fec25024751a72`, upstream
commit `f2eb930dda3779c6d852b72f3712aacd6e573ab1`) but for the three changes
below, each marked `TILO PATCH` in the source, and its `[lib]` building an
`rlib` only (last section). iroh 1.3.0, the newest release
when this was written, has the same code in every place patched. Pinned by
`crates/system/fs/tests/iroh_idle.rs`.

Tilo binds every endpoint with an empty relay map until someone could need to
dial the device, and empties it again five minutes after the last
collaborator leaves (`crates/session/shell/src/network/reachability.rs`,
tilo-fs's `bind_endpoint_with_key`). With no relays a net report is skipped
(`DirectAddrUpdateState::run`), yet the endpoint kept waking.

## The re-STUN tick waits for a relay (`src/socket.rs`)

The socket actor re-ran its address discovery every 20-26 s
(`periodic_re_stun_timer`) whether or not there was a relay to ask. With an
empty map the run only republishes "no report", so the tick woke the runtime
for nothing. Its `select!` arm is now disabled while the relay map is empty,
and `handle_relay_map_change` starts the timer afresh, so the stale deadline
of an unpolled timer does not fire beside the run a new relay starts. With
relays the cadence is unchanged.

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

## Built as an rlib only (`Cargo.toml`)

Upstream also builds a `cdylib` (for wasm), and Cargo leaves the hash out of
a path package's library filenames when it builds a dylib or cdylib. Once
the crate is a path dependency, two builds of it in one Cargo run (two
feature sets or profiles, as a full-workspace `cargo nextest run` makes)
write the same `target/debug/deps/libiroh.rlib` and `.so` ("output filename
collision"), and dependents then fail with E0460/E0463. Nothing in Tilo loads
the cdylib, so the `[lib]` builds `rlib` only.

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
> and send `SetHomeRelay(false)` to the active relays. Also: with an empty
> map, a second link change with the same `UpdateReason` sets an equal
> `(None, why)` net report, which `Watchable::set` does not notify, so
> local direct addresses are only refreshed by the periodic tick; collect
> them in `re_stun` when the map is empty.
