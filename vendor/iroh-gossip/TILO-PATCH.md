# Tilo patch: sweep a topic's message cache only while it holds messages

crates.io `iroh-gossip` 0.101.0, verbatim (archive SHA-256
`4e1dc4b05f73e7a1b9e83b531eb63c3fd671b0af3aeb13b59c546dd7ca747515`, upstream
commit `2ce78afe09d89d41d123f28eac19bdc831609cc8`) but for the change below,
marked `TILO PATCH` in `src/proto/plumtree.rs`, and its `[lib]` building an
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

> **Plumtree's `EvictCache` timer re-arms forever, even with empty caches.**
> `State::handle` arms it on the first event and `on_evict_cache_timer`
> always re-arms it, so an idle topic wakes every `cache_evict_interval` for
> its whole life. Proposed: arm it when the first entry is inserted into
> `cache`/`received_messages` (tracked by a pending flag), and re-arm in
> `on_evict_cache_timer` only while either is non-empty.
