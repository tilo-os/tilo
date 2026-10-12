# Tilo patch: wake on a resume, not on a 15 s clock poll

crates.io `netwatch` 0.19.1, verbatim (archive SHA-256
`4d9cbe01741347ef750d743d6690603f5eed8341e679fb51c8e629337aa11976`, upstream
commit `051ab8761006d7f2155e34a49f6bb881b582d5ab`) but for the change below,
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
