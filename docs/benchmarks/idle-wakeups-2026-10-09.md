# Idle wakeups, headless, 2026-10-09

The first measured number for "how often does an idle desktop wake the
machine". The raw tables are in `idle-wakeups-2026-10-09.json`. The current
value comes from the scheduled lane's `wakeups.*` records, not from this page
(see [README.md](README.md)).

## Method

`crates/session/shell/tests/idle_wakeups.rs`, run as
`cargo test --locked -p shell --test it idle_wakeups:: -- --exact --nocapture`
through **Dev / focused test** on the shared x86-64 `runner` class (KVM
guest, debug build, headless backend, lavapipe):

1. The production `shell` binary starts headless on a private runtime with a
   completed identity (no onboarding), a saved session holding one Notes tab,
   `TILO_IDLE_LOCK_SECS=0` and `TILO_IDLE_BLANK_SECS=0`, and
   `TILO_PAINT_STATS=1`.
2. A Terminal opens through the desktop's action `window.new.terminal`. Both
   apps are framework apps the compositor hosts in its own process.
3. The test waits for GStreamer's one-off plugin scan (a child,
   `gst-plugin-scan`, building a registry into the fresh cache) to finish,
   then for 5 s more without one.
4. `tilo_test_env::idle::idle_wakeups` samples every thread of the
   compositor's process tree from `/proc/<pid>/task/<tid>/{status,stat}`
   every 250 ms for 10 s. A wakeup is a context switch, voluntary or
   involuntary; CPU is utime+stime.

The collaboration plane is measured separately by
`shell::network::idle_quiet_tests` (two converged desktops on
loopback, one minute): the same counters for every thread of the test process
except the one driving the simulated 10 Hz frame loops.

## Result

Desktop with Notes and a Terminal, whole process tree:

| run | source | wakeups/s | CPU, % of a core | frame loop ticks/s |
| -- | -- | --: | --: | --: |
| 89704 | 9b730752b9 | **60.3** | 2.1 | 0 |
| 89702 | 9b730752b9 | 247.7 | 10.4 | (spin ended in the window) |
| 89409 | 0ac5ff2f54 | 1684.9 | (not sampled yet) | spinning (1752 per paint-stats line) |
| 89647 | 303d0009d0 | 2008.2 | 86.4 | 945.9 |

The spread is one bug, a widget's overdue wake deadline that kept the idle
frame loop spinning: the Terminal's record is still `Saving` when
it last draws, which leaves a wake deadline 100 ms after that sample that the
compositor never re-samples. The frame loop then spins without painting until
an unrelated paint makes the Terminal draw, and every iteration wakes the
settings reader. Runs 89409 and 89647 measured inside the spin (and before
the test waited for the plugin scan, which added 92/s), 89702 caught its end
and 89704 measured after it.

With the loop parked (89704), by thread:

| wakeups/s | thread | what it is |
| --: | -- | -- |
| 20.8 | `tilo-collab-broker-*` (2) | collaboration broker drain and capnp service |
| 19.9 | `tilo-audio-output-authority` | `audio_output.rs` revalidates every 50 ms, forever |
| 10.4 | `tilo-net-rt-0`, `-1` | the network actor's tokio runtime |
| 3.8 | `ui-collab-shovel` | ui-core collaboration broker shovel |
| 2.0 | `tilo-identity-client` | identity broker client |
| 1.0 | `tilo-settings-live` | settings file stamp poll, once a second |
| 1.0 | `tilo-settings-read` | settings refresh, one per frame-loop wake |
| 0.5 | `tilo-auto-brightness` | 2 s auto-brightness poll |
| 0.4 | `tilo-session-audio-policy` | session audio policy |
| 0.3 | `tilo-fs-authority-publish` | fs authority publication |
| 0.2 | `tilo-secrets-bus` | secrets bus |
| **60.3** | | |

While that spin runs (89647) the same desktop adds `tilo-settings-read`
1387/s and the compositor's main thread at 78 % of a core with 444
preemptions/s, and `tilo-net-rt-0` rises from 4/s to 25/s.

Two converged desktops on loopback (collaboration plane only): 9.9, 10.5 and
11.6 wakeups/s (runs 89705, 89410, 89703), all of it the two network runtimes'
workers; zero application messages and zero repaints, as the collaboration plane requires.

## What the number does not include

DRM-only services (power at 4 Hz; hotplug; the rotation sensor),
out-of-process apps (the per-app key poll; child supervision),
the standalone runtime daemon and the OS egress reaper.
Those are held by thread probes in their own crates.

## Budgets set from it

- `IDLE_WAKEUP_BUDGET` 3000/s and `LOOP_TICK_BUDGET` 1500/s in
  `idle_wakeups.rs`: the spinning measurement plus a half, because whether a
  run sees the spin depends on the wall clock and a spin's rate on the
  runner. Once the spin is fixed they drop to 75/s (60/s plus a quarter) and 1/s.
- `IDLE_WAKEUP_BUDGET` 15/s in `idle_quiet_tests.rs`: 11.6/s plus a quarter.

Each later fix lowers the matching budget in its own pull request: the
audio-output poll (−20/s), the collaboration broker threads (−21/s), the
network runtime (−10/s) and the rest of the table.

## Later fixes

| fix | thread | before | after | parked desktop, whole tree |
| -- | -- | --: | --: | --: |
| audio-output watchdog parks | `tilo-audio-output-authority` | 20.1/s (run 89961) | 0.0/s (run 89960) | 31.9/s (run 89960) |
| collaboration drain renamed | `tilo-collab-drain` | 0.0/s (run 90040) | 0.0/s (run 90187) | unchanged |
| broker service thread parks | `tilo-collab-broker-capnp` | 20.1/s (run 90455) | 0.0/s (run 90830) | spinning, see below |
| overdue widget deadline answered once | `tilo-settings-read` and the frame loop | 2989/s, 1495 ticks/s (builder) | 1.0/s, 0 ticks/s (builder) | 13.2/s (builder) |

The overdue widget deadline: a widget may keep an overdue wake deadline until a paint consumes
it (the Terminal's save status does), while a due deadline alone does not
paint. The frame loop now answers such a deadline once: one that was already
due when a tick started no longer wakes the park. Measured on the shared
builder, since CI had not run it yet: the gates drop to 16.5/s (13.2/s plus a
quarter) and 1 tick/s.

The audio-output watchdog re-checks an open output's admission every 50 ms as
before, and blocks on a condition variable while no output is open; `open`
wakes it. The parked budget becomes 31.9/s plus a quarter, 40/s, once
the spin no longer hides it.

Renaming the drain split the collaboration broker's 20.8/s by renaming its drain
thread (`tilo-collab-drain`; the probe's 15-byte names had shown both
threads as `tilo-collab-bro`). All of it is the capnp service thread,
`tilo-collab-broker-capnp`: 19.9/s in run 90040 and 19.7/s in run 90187,
two context switches for each turn of a loop that re-armed a 100 ms tick
with nothing due. Removing that tick left the thread at 2.3/s (run 90061),
its 1 s membership refresh, which the next fix replaces with change signals. The drain was already parked here: this
desktop holds no encrypted channel. On a session that does, it pumped
channel governance once a second; it now waits for the governance
service's own deadline (a steward's 1 s policy re-check, a member's
backoff, nothing for a member holding its keys), so a member device's
drain wakes only for frames. The gate cannot see that, so the budget
does not move for it.

Parking the broker's service thread takes it to 0.0/s. It re-checks what it
admitted (membership surfaces, native context subscribers, a steward's
channel readers) when an input announces a policy change and at the first
grant or certificate expiry it read, instead of once a second, and its turn
parks with nothing due, so the 100 ms tick goes with it. A first cut that
announced on every tree write measured 2.7/s (run 90634): the frame loop
writes the tree on each pass (`push_room_changes`, `replicate_room_records`,
the overlay host's update), and the broker coalesced those announcements to
one re-check a second, the old clock's cost. A write now announces only
when it changed what authorization reads (owners, rosters, grants,
permission profiles, tasks, admitted devices, stewarded rooms). The
whole-tree budget does not move: run 90830 still spun at 1215/s on the overdue
widget deadline, which the 3000/s spin budget covered; the parked budget
dropped once that was fixed.
