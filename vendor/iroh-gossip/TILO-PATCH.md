# Tilo patches: stop topic timers that have no work, reach every neighbour, broadcast only with one

crates.io `iroh-gossip` 0.101.0, verbatim (archive SHA-256
`4e1dc4b05f73e7a1b9e83b531eb63c3fd671b0af3aeb13b59c546dd7ca747515`, upstream
commit `2ce78afe09d89d41d123f28eac19bdc831609cc8`) but for the changes below,
marked `TILO PATCH` in `src/proto/{plumtree,hyparview}.rs`, and its `[lib]` building an
`rlib` only (last section). 0.101.0 is the newest
release. Pinned by `crates/system/fs/tests/iroh_idle.rs`
(`gossip_sweeps_a_topic_cache_only_while_it_holds_messages`) and the
plumtree unit test `cache_sweep_runs_only_while_something_is_cached`.

Plumtree armed its cache sweep (`Timer::EvictCache`) on the first event a
topic handled and re-armed it on every sweep for the life of the topic,
whether or not anything was cached. The compositor sweeps every 10 s
(`broadcast_config`), and three topics per room: a desktop whose
collaborator had left kept waking about 0.3 times a second for nothing
(focused run 95964), and quitting and rejoining a topic to stop it doubles
the timers instead, since the old topic's pending timers fire into the new
state.

The sweep is now armed when an entry goes into the message cache or the
received-message set and none is pending, and re-armed only while either
still holds entries. A topic idle for `message_id_retention` (90 s by
default) stops sweeping; the next message arms it once. Cadence and
retention while messages flow are unchanged, and there is never more than
one pending sweep per topic.

## Membership shuffle only with active neighbors

HyParView also armed `DoShuffle` on the first event and rearmed it forever,
including after the last neighbor left. A shuffle without an active neighbor
sends nothing, but its 60-second timer still wakes the runtime for every topic.
The shell's 20-second rested-desktop measurement could miss that boundary or
straddle it; draining Plumtree's caches does not stop membership timers.

The shuffle is now armed only with an active neighbor. A pending timer may
expire once after departure, then stops; a later join rearms it. A peer that
returns before expiry reuses the pending timer. Live-neighbor shuffle cadence,
passive-view recovery and wire messages are unchanged. The public protocol-state
regression `gossip_stops_shuffling_after_its_last_peer_leaves_and_restarts_on_join`
in `crates/system/fs/tests/iroh_idle.rs` verifies departure, both return timings,
and continued shuffling with a live neighbor without wall-clock sleeps.

## Neighbours-only messages reach lazy neighbours

`Scope::Neighbors` is documented as "broadcast only to the immediate
neighbors", but `broadcast` pushed it to the eager peers only. A neighbour
that once sent a duplicate swarm message is pruned to the lazy set, which in
a triangle of three peers happens to one link at the first broadcast; from
then on nothing neighbours-only crossed that link, as the lazy path (`IHave`
and `Graft`) carries swarm messages only. Tilo announces device identities
neighbours-only, so of three devices of one person two stopped
authenticating each other after a restart. Such a message now goes to every
direct neighbour, eager or lazy (`a_neighbours_only_message_reaches_lazy_neighbours_too`, and
through the public topic state `gossip_neighbours_only_messages_cross_a_pruned_link` in
`crates/system/fs/tests/iroh_idle.rs`).

## No broadcast without a neighbour

`broadcast` cached a swarm message and armed the cache sweep with no eager
or lazy peer to send it to. Nobody could graft it, and a neighbour that
comes later is never offered an older message, so a desktop whose last
collaborator had left kept sweeping for every local change it broadcast
long after. With no neighbour, `broadcast` now does nothing
(`a_broadcast_nobody_hears_arms_nothing`, and through the public topic state
`gossip_sweeps_a_topic_cache_only_while_it_holds_messages` in
`crates/system/fs/tests/iroh_idle.rs`). Tilo restates what a new neighbour
needs when it joins.

## The address lookup sweeps only while it holds addresses (`src/net/address_lookup.rs`)

`GossipAddressLookup`, which keeps the addresses learned from `Join` and
`ForwardJoin` for five minutes, swept them on a 30 s interval for the life of
the endpoint, empty or not: every endpoint with gossip woke every 30 s. Its
task now sleeps until the oldest address expires (at least 30 s after the
last sweep, as before), and while it holds none waits for `add` to wake it.
Addresses are kept and evicted as before; the lone idle hosting device's
gate in the shell (`one_host_tests::idle_host`) holds it.

## Built as an rlib only (`Cargo.toml`)

Upstream also builds a `cdylib`, and Cargo leaves the hash out of
a path package's library filenames when it builds a dylib or cdylib. Once
the crate is a path dependency, two builds of it in one Cargo run (two
feature sets or profiles, as a full-workspace `cargo nextest run` makes)
write the same `target/debug/deps/libiroh_gossip.rlib` and `.so` ("output filename
collision"), and dependents then fail with E0460/E0463. Nothing in Tilo loads
the cdylib, so the `[lib]` builds `rlib` only.

## Upstream

Prepared for an upstream issue/PR against `n0-computer/iroh-gossip` (not
opened):

> **`GossipAddressLookup` sweeps every 30 s for the life of the endpoint.**
> Proposed: sleep until the oldest entry's retention ends, and wait for the
> next `add` while the map is empty.
>
> **Plumtree's `EvictCache` timer re-arms forever, even with empty caches.**
> `State::handle` arms it on the first event and `on_evict_cache_timer`
> always re-arms it, so an idle topic wakes every `cache_evict_interval` for
> its whole life. Proposed: arm it when the first entry is inserted into
> `cache`/`received_messages` (tracked by a pending flag), and re-arm in
> `on_evict_cache_timer` only while either is non-empty.
