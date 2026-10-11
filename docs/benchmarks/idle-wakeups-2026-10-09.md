# Idle wakeups, headless, 2026-10-09 (TIL-614)

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
`shell::network::idle_quiet_tests` (TIL-613's two converged desktops on
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

The spread is one bug, TIL-696: the Terminal's record is still `Saving` when
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
| 0.5 | `tilo-auto-brightness` | 2 s auto-brightness poll (TIL-205) |
| 0.4 | `tilo-session-audio-policy` | session audio policy |
| 0.3 | `tilo-fs-authority-publish` | fs authority publication |
| 0.2 | `tilo-secrets-bus` | secrets bus |
| **60.3** | | |

While TIL-696 spins (89647) the same desktop adds `tilo-settings-read`
1387/s and the compositor's main thread at 78 % of a core with 444
preemptions/s, and `tilo-net-rt-0` rises from 4/s to 25/s.

Two converged desktops on loopback (collaboration plane only): 9.9, 10.5 and
11.6 wakeups/s (runs 89705, 89410, 89703), all of it the two network runtimes'
workers; zero application messages and zero repaints, as TIL-613 requires.

## What the number does not include

DRM-only services (power at 4 Hz, TIL-611; hotplug; the rotation sensor),
out-of-process apps (the per-app key poll, TIL-598; child supervision, TIL-601),
the standalone runtime daemon (TIL-607) and the OS egress reaper (TIL-612).
Those are held by thread probes in their own crates.

## Budgets set from it

- `IDLE_WAKEUP_BUDGET` 3000/s and `LOOP_TICK_BUDGET` 1500/s in
  `idle_wakeups.rs`: the spinning measurement plus a half, because whether a
  run sees TIL-696 depends on the wall clock and a spin's rate on the
  runner. TIL-696 dropped them to the parked measurement plus a quarter
  and 1/s (see "TIL-696" below).
- `IDLE_WAKEUP_BUDGET` 15/s in `idle_quiet_tests.rs`: 11.6/s plus a quarter.
  TIL-690 raised it to 21/s, the shell shard's measured p95 plus a quarter
  (see "The collaboration-plane gate in the shell shard").

Each later fix lowers the matching budget in its own pull request: the
audio-output poll (−20/s), the collaboration broker threads (−21/s), the
network runtime (−10/s) and the rest of the table.

## Later fixes

| fix | thread | before | after | parked desktop, whole tree |
| -- | -- | --: | --: | --: |
| TIL-704 | `tilo-audio-output-authority` | 20.1/s (run 89961) | 0.0/s (run 89960) | 31.9/s (run 89960) |
| TIL-706 | `tilo-collab-drain` | 0.0/s (run 90040) | 0.0/s (run 90187) | unchanged |
| TIL-707 | `tilo-collab-broker-capnp` | 20.1/s (run 90455) | 0.0/s (run 90830) | spinning, see TIL-696 |

TIL-704: the watchdog re-checks an open output's admission every 50 ms as
before, and blocks on a condition variable while no output is open; `open`
wakes it. The parked budget becomes 31.9/s plus a quarter, 40/s, once
TIL-696 lets the gate use it.

TIL-706 split the collaboration broker's 20.8/s by renaming its drain
thread (`tilo-collab-drain`; the probe's 15-byte names had shown both
threads as `tilo-collab-bro`). All of it is the capnp service thread,
`tilo-collab-broker-capnp`: 19.9/s in run 90040 and 19.7/s in run 90187,
two context switches for each turn of a loop that re-armed a 100 ms tick
with nothing due. TIL-690 (#3983) removes that tick; with it the thread
measured 2.3/s (run 90061), its 1 s membership refresh, which TIL-707
replaces with change signals. The drain was already parked here: this
desktop holds no encrypted channel. On a session that does, it pumped
channel governance once a second; it now waits for the governance
service's own deadline (a steward's 1 s policy re-check, a member's
backoff, nothing for a member holding its keys), so a member device's
drain wakes only for frames. The gate cannot see that, so the budget
does not move for it.

TIL-707 takes the capnp service thread to 0.0/s. It re-checks what it
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
whole-tree budget does not move: run 90830 still spun at 1215/s (TIL-696),
which the 3000/s spin budget covers; the parked budget drops with TIL-696.

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

### TIL-696: the frame loop stops spinning

An in-process app whose record was still `Saving` at its last frame left a
wake deadline that only its next draw moved. `EmbeddedApp::update` now
samples it again on the compositor's pump, without drawing: the deadline
moves on while the save runs, and goes away (with one frame to show it)
once the save is durable. The loop's two per-tick continuity policy reads
(`window_viewers`, `publish_window_views`) take the loop's cached policy, so
an iteration no longer queues a settings refresh.

| run | source | wakeups/s | CPU, % of a core | frame loop ticks/s |
| -- | -- | --: | --: | --: |
| 89647 (before, spinning) | 303d0009d0 | 2008.2 | 86.4 | 945.9 |
| 89957 | d6018b47e5 | 51.4 | 1.2 | 0 |
| 92406 | 72e3096677 (on TIL-704/707/702/740) | 7.30 | 0.2 | 0 |
| 92444 | 72e3096677 | 7.60 | 0.3 | 0 |
| 92445 | 72e3096677 | 7.30 | 0.2 | 0 |
| 92498 (shell shard, beside other tests) | 527f761a98 | 10.29 | 0.1 | 0 |

The gate runs in the shell shard beside other tests, where its neighbours
preempt the collaboration shovel and the identity client, so its budget is
the shard's 10.3/s plus a half, 15/s, and 1 tick/s for the loop. What is
left: `ui-collab-shovel` 3.4/s, `tilo-identity-client` 2.0/s, the network
runtime about 1/s, and a few tenths from the fs authority, session audio and
secrets bus.

### TIL-752: upkeep and gossip's sweep wait for a peer

What TIL-702 left on loopback, about 1/s, was the network actor's 5 s
upkeep tick and iroh-gossip's 10 s cache sweep on each of the desktop's
three topics (the compatibility topic, its room, the person's continuity
topic). With nobody to talk to neither has work. The upkeep tick is now
armed only while there is a peer, a workspace host, a governed room's
relay route, a remote action held for a peer or a failed subscription, and
the address it refreshed every tick follows iroh's address watcher instead.
iroh-gossip arms a topic's sweep on its first message or neighbour and never
stops it, and a lone desktop broadcast into its empty topics on every local
change, so a topic now hands gossip nothing until it has had a neighbour.
The 600 s host-checkpoint tick is armed only while a host session exists.

Focused runs, blocking (voluntary) switches per second:

| gate | main `f744af88ad` (92709-92713) | branch (92724, 92787-92796) |
| -- | --: | --: |
| lone desktop, all threads | 0.90-1.05 | 0.30-0.40 |
| lone desktop, `tilo-net-rt` | 0.85-0.95 | 0.25-0.35 |
| idle pair, all threads | 6.12-6.98 | 5.84-7.17 |

What the lone desktop still does, from a tracing probe (run 92833): in the
20 s window only two events, both iroh timers that Tilo cannot reach through
iroh's API, netwatch's 15 s wall-clock check and the socket's 20-26 s
re-STUN tick (a no-op with no relays). They cost a worker eight switches in
that window. A desktop that has had a peer keeps its topics' sweeps until it
leaves them, since iroh-gossip has no way to stop one. Each topic also keeps
iroh-gossip's 60 s membership shuffle, outside the gate's window.

The lone gate's budget is now 0.6/s (p95 0.45/s plus a quarter). The idle
pair is gated on blocking switches too, at 12/s with a 1 % CPU bound.

### Relays rest again, and invites name a home relay

Waking the relays used to be sticky: after any collaboration, even an
invitation issued and cancelled, an idle desktop ran iroh's net report every
20-26 s again for the rest of the session. `Reachability` now records what
holds the relays up: the network actor reports the remote parties it is
connected to or keeps dialling (a peer it dials or one on its topics, a host,
a governed room's relay, one of the person's devices, a streamed window, a
held remote action, an enrolment, a bound Messages account), and the session
what it has recorded (a joined room, a peer named in a room, a relay-governed
or continuation room, an open invitation, another of the person's devices),
re-read at most once a second while the compositor ticks. Five minutes after
the last of these ended, and after the last wake, `Endpoint::remove_relay`
empties the map, and net report, QAD and port mapping stop. iroh's API cannot
close the home relay it chose, so that connection and its 15 s ping stay
until the endpoint closes; the address keeps naming it.

A peer whose name was removed from a room no longer counts: its certified
identity row stays as revocation evidence, but it is not a member.

Opening the invite composer starts the relays, and a direct invite waits up
to 2 s for a home relay, so the token names one (a loopback relay without
QAD is picked in about 0.2 s, one whose QAD never answers in about 3 s).

Focused run 95964: once its collaborator had left and been taken out of the
room, the desktop rested its relays and then blocked 0.65 times a second,
against 0.30/s for a lone desktop that never had a peer (run 95857). The
difference is gossip's sweeps of the three topics the pair linked, which
iroh-gossip cannot stop; `reachability_tests` gates it at the lone budget
plus those sweeps, 0.9/s.

### iroh's own timers stop when they have nothing to do

What was left on the lone desktop, the rested one and an idle `fs serve`
was iroh's: netwatch's 15 s wall-clock poll, the socket's 20-26 s re-STUN
tick with no relays, the 15 s ping of a home relay kept after the relays
were removed, and iroh-gossip's 10 s cache sweep for the life of a topic.
No release changes them (iroh 1.3.0, netwatch 0.19.3 and iroh-gossip
0.101.0 have the same code), so the locked versions are vendored with one
patch each (`vendor/iroh`, `vendor/netwatch`, `vendor/iroh-gossip`, each
with a `TILO-PATCH.md`). The re-STUN tick waits for a relay, emptying the
map releases the home relay, netwatch waits on a timerfd the kernel cancels
on resume or a clock step (its poll could not see a sleep on Linux, where
`Instant` stands still while suspended), and gossip sweeps only while
something is cached. Network changes still arrive through netlink:
`crates/system/fs/tests/iroh_idle.rs` sees `lo` come up and an address come
and go in an endpoint's own network namespace after about 270 ms each.

Focused runs, blocking (voluntary) switches per second:

| gate | main `7180919b15` (96742-96744) | branch (96812, 96954-96958) |
| -- | --: | --: |
| lone desktop, all threads | 0.35-0.40 | 0.20 |
| lone desktop, `tilo-net-rt` | 0.30-0.35 | 0.15 |
| desktop whose collaborator left, all threads | 0.60-0.65 | 0.05 |
| desktop whose collaborator left, `tilo-net-rt` | 0.55-0.65 | 0.00 |
| endpoint with its relays removed (`iroh_idle`, 30 s) | | 0.00 |
| idle `fs serve` (ci-filesystem; main 96747-96779, branch 97069) | 0.65 | 0.30 |

The rested desktop is now measured once gossip's caches have drained (100 s
after its relays rest; a real desktop rests them five minutes after the last
collaborator), and gated at 0.10/s instead of the lone budget plus 0.3/s of
sweeps. The lone budget is 0.25/s (p95 0.20/s plus a quarter), and an idle
`fs serve`'s 0.5/s, between what it does now and what the iroh timers cost. What the lone
desktop's network runtime still does 15 to 35 s after it starts is not
periodic: the rested desktop, two minutes into its run, shows none.

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
