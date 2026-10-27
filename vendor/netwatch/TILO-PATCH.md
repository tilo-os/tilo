# Tilo patches: wake on a resume, not on a 15 s clock poll; let the kernel drop address-lifetime refreshes

crates.io `netwatch` 0.19.1, verbatim (archive SHA-256
`4d9cbe01741347ef750d743d6690603f5eed8341e679fb51c8e629337aa11976`, upstream
commit `051ab8761006d7f2155e34a49f6bb881b582d5ab`) but for the three changes below,
marked `TILO PATCH` in the source. netwatch 0.19.3, the newest release when
this was written, has the same actor loop. Pinned by
`crates/system/fs/tests/iroh_idle.rs`.

The network monitor's actor (`src/netmon/actor.rs`) read the clock every
15 s (`POLL_WALL_TIME_INTERVAL`) to notice that the machine had slept: a gap
of over 22.5 s between ticks counts as a wake from sleep and forces a major
network change. On Linux that poll cannot see a sleep. `Instant` is
`CLOCK_MONOTONIC`, which stands still while the machine is suspended, so the
interval resumes where it left off and never measures the gap. It only woke
every iroh endpoint four times a minute. Network changes themselves already
arrive as netlink events (`src/netmon/linux.rs`: address, route and rule
groups), debounced by 250 ms; that path is unchanged.

`src/netmon/clock_change.rs` (new) arms a `timerfd` on `CLOCK_REALTIME` with
`TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET` at the end of time. The kernel
cancels it whenever the realtime clock is set and on every resume
(`timekeeping_resume` calls `timerfd_resume`), and it does nothing
otherwise. On a cancellation the actor compares `CLOCK_BOOTTIME -
CLOCK_MONOTONIC`, the time spent suspended, with the last value: if it grew
by a second or more the machine slept and the actor treats it as before
(`pending_time_jump`); a clock step alone changes nothing. Where no timerfd
can be made (other systems, or a refused call) the actor polls as before.

So on Linux the actor sleeps until the network or the clock changes, and a
resume is now noticed, where the poll missed it.

Since tokio 1.53.3 deprecates the safe `AsyncFd` constructors as unsound,
the timerfd is registered with the unsafe `AsyncFd::register_with_interest`.
The AsyncFd owns the descriptor, which meets its contract.

## Upstream

Prepared for an upstream issue/PR against `n0-computer/net-tools` (not
opened):

> **netmon's wall-time poll wakes every 15 s and cannot detect sleep on
> Linux.** `check_wall_time_advance` measures `tokio::time::Instant`, i.e.
> `CLOCK_MONOTONIC`, which does not advance during suspend, so the jump it
> looks for never appears after a resume. Proposed: on Linux, replace the
> interval with a `timerfd` on `CLOCK_REALTIME` armed with
> `TFD_TIMER_CANCEL_ON_SET` (cancelled on clock set and on resume via
> `timerfd_resume`), and tell a resume from a clock step by the growth of
> `CLOCK_BOOTTIME - CLOCK_MONOTONIC`; keep the poll as a fallback and on
> other platforms.

## Address-lifetime refreshes no longer wake the process

The monitor subscribes to the kernel's address notifications and, since
upstream's own cache, ignores an `RTM_NEWADDR` that repeats an address it
knows. Ignoring one still costs a wakeup: reading it is what wakes the
process. On a wifi network a router that advertises its prefix every few
seconds makes the kernel send exactly such a lifetime-only `RTM_NEWADDR` to
every subscriber each time, 0.3 wakeups a second for as long as the process
lives, in every process with an endpoint. `src/netmon/linux.rs` now attaches
a classic BPF program to the netlink socket (`SO_ATTACH_FILTER`) that makes
the kernel drop IPv6 `RTM_NEWADDR` messages with the interface, prefix
length, flags, scope and address of one the monitor already knows. New
addresses, changes of flags or prefix length, removals and every other
message still pass. The program is rebuilt when the known set changes (it
lists at most 32 addresses; beyond that the monitor reads every message as
before). A test runs the program on a datagram socket, where the kernel sees
a message exactly as on a netlink socket, and checks what is dropped and what
passes; `crates/system/fs/tests/iroh_idle.rs` and the shell's lone-desktop
wakeup test refresh an address's lifetimes five times a second and assert the
endpoint still sleeps.

### Upstream

> **netmon wakes on every IPv6 lifetime refresh.** A router sending router
> advertisements every few seconds makes the kernel send `RTM_NEWADDR` for
> each address of the prefix to every `RTMGRP_IPV6_IFADDR` subscriber; netmon
> reads and discards each one as "already known", at a wakeup apiece. Proposed:
> attach a BPF filter to the netlink socket that drops `RTM_NEWADDR`
> repeating a known (interface, prefix length, flags, scope, address).

## A change made while the monitor subscribes is no longer lost

`Monitor::new` takes the interface snapshot (`State::new` in `Actor::new`)
first and only then starts the route monitor, whose task subscribes to
netlink later, when the runtime first runs it. A change in between reaches no
socket, so the snapshot stays stale until some unrelated event: an interface
brought up right after the endpoint binds was not noticed at all. The
address dump added above widens the gap for addresses, since an address the
dump already lists is read afterwards as a repeat. Once subscribed and done
with the dump (at start and after every reconnect), the route monitor now
sends the actor one `Change`, and the actor compares a fresh snapshot with
the old one as for any event. Delaying the subscription by 700 ms made
`iroh_idle::an_endpoint_hears_of_network_changes_through_netlink` fail every
time without this and pass with it.

### Upstream

> **netmon can miss a change made while it starts.** `Actor::new` reads the
> interface state before the route monitor's task has subscribed to netlink,
> and nothing re-reads it until the next event, so a change in that window
> (or during a reconnect) is lost. Proposed: once subscribed, have the route
> monitor send one `NetworkMessage::Change` so the actor re-reads the state.
