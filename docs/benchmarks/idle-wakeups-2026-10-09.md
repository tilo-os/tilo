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
  TIL-690 raised it to 21/s, the shell shard's measured p95 plus a quarter
  (see "The collaboration-plane gate in the shell shard").

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

### TIL-702: the network runtime on an installed desktop

The headless gate above sees `tilo-net-rt` at 2.9/s (run 91403): it has no
call device table and no third runtime thread. The VM does. In vm-acceptance
R01 (no apps open), by thread, before (run 91456, dev disk `35c21096db`) and
after (run 91460, the same disk with this branch's shell swapped in, which
includes #4044's gossip sweep):

| thread | before | after |
| -- | --: | --: |
| `tilo-net-rt-2` (blocking pool) | 15.40/s | gone |
| `tilo-net-rt-1` | 6.57/s | 3.73/s |
| `tilo-net-rt-0` | 6.50/s | 2.97/s |
| `tilo-net-rt` group | **28.47/s, 1.03 % of a core** | **6.70/s, 0.23 %** |
| whole compositor's threads | 41.3/s | 20.0/s |

`tilo-net-rt-2` was the call device table (`network/native_media/devices.rs`):
installed on the network runtime whenever the person has a device identity,
it ran a blocking PipeWire registry round trip every second from session
start, call or no call. That is also why `wireplumber` (0.43 %) and
`pipewire` (6.9/s) were busy in run 91456 and are not in run 91460
(`pipewire` 0.7/s). Only a call reads the table, so a call now holds a
`Watch` from when it starts preparing until it ends, and the loop parks on a
`Notify` otherwise.

The workers' drop is #4044: gossip's per-topic cache sweep every second.

What remains, named by counting tracing spans and events per thread in a
lone desktop's process (a temporary probe on the TIL-702 branch, focused run
91459): on loopback 0.95/s, the 5 s upkeep tick and the 10 s sweep. With the
public relays (`Reach::Internet`, as the VM runs) 6.0/s, two thirds of it
inside iroh's `net_report` `QADv4` span: every 20-26 s iroh re-runs its net
report and opens fresh QUIC address-discovery connections to the relays
(handshakes, then closes). Getting under TIL-702's 2/s target needs that
deferred until something needs reachability (the issue's step 2), which is
left for a follow-up.

`idle_quiet_tests::a_lone_idle_desktop_keeps_its_network_runtime_asleep`
gates the loopback case at 2/s (measured 1.05/s, focused run 91528). The
call table's parking is held by
`devices::tests::the_refresh_loop_runs_only_while_a_call_watches`.

### TIL-740: relays wait until something needs reachability

TIL-702's residue was iroh's net report: with a non-empty relay map, iroh
re-runs it every 20-26 s (QAD handshakes to every relay, a port-mapping
probe) and keeps a home relay connection alive. iroh skips all of that while
the relay map is empty, so the endpoint now binds with N0's preset and
`RelayMode::Custom(RelayMap::empty())`, and the default relays wait in
`network::reachability::Reachability` until something needs this device
reachable: a shared room, an open invitation or another of the person's
devices recorded at session start; a dial to a peer or host, an enrolment,
a call, a send to the person's devices or a peer reaching a topic in the
actor; an issued invitation; a bound Messages account. Then
`Endpoint::insert_relay` adds them, with the endpoint, its key and its
connections unchanged. Waking is sticky for the session.

The lone-desktop gate now runs on that path (`Reach::LoopbackHoldingRelays`)
and asserts the relays are still held back after its idle window: 1.05/s
(focused run 92076), the same as loopback. `reachability_tests` holds the
startup predicate and a pair of desktops that hold their relays back, still
sync, and then both wake them.

## The collaboration-plane gate in the shell shard

`idle_quiet_tests` (two converged desktops) had a 15/s budget, set from
focused runs. It runs in the pull-request shell shard, and there it failed
across unrelated PRs. All of the excess was `tilo-net-rt`, which none of
those PRs touched.

| where | runs | wakeups/s |
| -- | -- | -- |
| focused, main `f58efc9e86` | 90118 | 11.85 |
| focused, main `f238447596` | 90372, 90373, 90374, 90375 | 12.41, 10.80, 10.95, 10.23 |
| focused, #3983 | 90117, 90376, 90377, 90378 | 11.42, 10.83, 12.50, 11.61 |
| shell shard, #3983 | 89970, 90055, 90190, 90601 | 15.59, 15.52, 16.16, 15.82 |
| shell shard, other PRs | #3982, #4006, #4009 | 15.31, 16.80, 15.18 |

Main and #3983 measure the same when focused, so the code is not the
difference; the shard environment is. It is not contention either. Check run
90601 gave the test the pool to itself (`threads-required = "num-cpus"`) and
still measured 15.82/s, with 0.7/s of involuntary switches. The extra
wakeups are voluntary `tilo-net-rt` turns where the shard runs (its runner
and `tools/test-env.sh`).

By TIL-614's rule, measured p95 plus a quarter, set in the environment the
gate runs in: shard p95 16.8/s → **21/s**. TIL-702, the network runtime's
idle wakers, is what lowers it.
