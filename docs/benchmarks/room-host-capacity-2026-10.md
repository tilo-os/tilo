# Room host capacity and cost per collaborator, October 2026

What one public room host (the room service's persistent room: one relay
governing one room, as eu1 runs the Lounge) holds, where it stops, and what a
concurrent guest costs on the nodes we would run it on. The numbers size eu1's
caps, memory request and limit and its capacity alerts.

**Result.** One room serves 50 guests well (join p99 0.55 s, chat delivery
p99 1.2 s) and 100 acceptably (join p99 9.9 s, chat p99 5.5 s). Past about
250 members it stops admitting: joins time out after 60 s at 20 and at 5
arrivals a second alike, chat commits stall, and the host never uses more
than about 3 of its 40 cores, with no single thread busy. eu1's cap of 500
guests is unreachable on this relay; its 6 GiB limit is five times the most
memory any run used (1.1 GiB at 4050 offered sessions). Spectators on the
homepage embed's path are refused outright (every page is "not in workspace
membership"), so eu1 has no working spectators today. At 100-guest rooms a
concurrent guest costs USD 14-37 per 100 a month on Hetzner and USD 2,600 on
AWS, where egress is 97 % of it.

## Method

`tilo-room-load bench` (`crates/dev/room-load`) starts a room host as a child
process (a relay governing public rooms, serving browsers on plain loopback
HTTP as eu1's ingress forwards them, with a `/join` that pre-admits each guest
as the room service does) and drives headless browser guests against it over
the relay's real browser routes. Every guest is pre-admitted through `/join`,
redeems its one-time token, holds a certified session, publishes cursor
presence on the live plane's HTTPS fallback (30 Hz while moving, moving 30 %
of the time) and posts a 300-byte chat change every 60 s on average.
Spectators certify an in-memory device through `/spectate` as the embed does
(`embed` runs), or are pre-admitted as Viewers of their own
(`--spectator-members`, the `spect` and `caps` runs) so a watching session can
be measured at all (see "Spectators"). Each run ramps, then holds a minute;
the host's CPU and memory are read from `/proc` over its whole process tree
(the relay and the Automerge worker processes it starts, proportional set
size so shared pages count once), with the busiest thread's share.

Each report also says where the host's time went (`timings`, from the relay's
`tilo_relay::timing` target, which the bench's host enables): per newcomer the
wait for a replay slot, its capture, preparation, boundary, catch-up, cleanup
and how long it held the slot, by arrival tenth; per commit its wait and
durability; per kind of change (member and host checkpoints and deltas, and
which channel governance a member's checkpoint carries) its owner turn and
publication; and per second the room owner's queue, transaction and
publication time, its replay turns by kind, room lock waits and membership
reloads. An owner whose turns add up to 1000 ms in a second is saturated.

`tools/perf/room_capacity.py sweep` runs the plan below one run after another
on one runner, and `summarize` derives the per-guest figures and prices them
with `[room_node.*]` of `tools/perf/prices.toml`. The **Perf / room host
capacity** workflow (`.forgejo/workflows/perf-room-capacity.yml`, manual
dispatch) runs both:

```sh
fgj actions workflow run perf-room-capacity.yml -R tilo/tilo -r <branch> -f plan=full
```

Every run uses production's allocator setting (`MALLOC_ARENA_MAX=2`, which the
room service sets for its relay) and the image's build (thin LTO, 16 codegen
units, the relay's `agents` feature, as `room-service-image.yml` builds it).
Production eu1 was never loaded.

**Machine.** `runner-large`, Perf / room host capacity run 112979 at
`261677a92b`, 2026-10-14: AMD EPYC 9R14 (Zen 4) in a KVM guest, a cgroup quota
of 40 CPUs and 36 GiB, kernel 6.12.47. eu1's node is a Ryzen 9 7950X3D (Zen 4,
16 cores, 32 threads, 128 GB; the infra repository calls it an AX52, but those
are AX102 specifications). The driver shares the machine and used at most 1.4
cores and 553 MiB, so it never starved the host.

## Results

| run | guests joined | spectators | refused (full) | failures | join p50 / p99 ms | chat p99 ms | spectator join p99 ms | host CPU cores (busiest thread) | peak memory MiB | egress Mbit/s | driver cores |
|---|---|---|---|---|---|---|---|---|---|---|---|
| idle | 0/0 | 0/0 | 0 | 0 | 0 / 0 | 0 | 0 | 0.01 (0.00) | 40 | 0.0 | 0.0 |
| plain-g50 | 50/50 | 0/0 | 0 | 0 | 276 / 547 | 1220 | 0 | 0.46 (0.05) | 73 | 23.1 | 0.3 |
| plain-g100 | 100/100 | 0/0 | 0 | 0 | 2574 / 9936 | 5520 | 0 | 1.14 (0.06) | 122 | 65.6 | 0.8 |
| plain-g300 | 210/300 | 0/0 | 0 | 90 | 10423 / 57071 | 54933 | 0 | 2.72 (0.15) | 483 | 118.7 | 1.3 |
| plain-g500 | 237/500 | 0/0 | 0 | 141 | 13615 / 59148 | 61081 | 0 | 3.06 (0.21) | 542 | 130.0 | 1.1 |
| plain-g800 | 222/800 | 0/0 | 0 | 534 | 16652 / 51189 | 79216 | 0 | 2.87 (0.21) | 705 | 144.8 | 1.1 |
| rooms4-g50 | 50/50 | 0/0 | 0 | 0 | 1618 / 4124 | 2057 | 0 | 0.58 (0.05) | 77 | 23.4 | 0.4 |
| rooms4-g100 | 100/100 | 0/0 | 0 | 5 | 6950 / 19648 | 33755 | 0 | 2.42 (0.07) | 137 | 68.2 | 1.2 |
| rooms4-g300 | 107/300 | 0/0 | 0 | 204 | 3983 / 59680 | 62509 | 0 | 2.80 (0.18) | 394 | 74.5 | 1.0 |
| rooms4-g500 | 48/500 | 0/0 | 0 | 452 | 767 / 55758 | 35716 | 0 | 3.07 (0.24) | 564 | 52.4 | 0.8 |
| rooms4-g800 | 66/800 | 0/0 | 0 | 561 | 3699 / 41896 | 64913 | 0 | 3.04 (0.45) | 857 | 58.2 | 0.7 |
| cap500-g500 | 249/500 | 0/0 | 0 | 104 | 19074 / 59208 | 62964 | 0 | 2.94 (0.14) | 576 | 144.9 | 1.0 |
| cap500-g800 | 231/800 | 0/0 | 300 | 246 | 18271 / 52040 | 70926 | 0 | 3.09 (0.28) | 724 | 148.9 | 1.2 |
| ramp5-g300 | 284/300 | 0/0 | 0 | 16 | 7981 / 56759 | 88687 | 0 | 3.01 (0.09) | 377 | 153.8 | 1.4 |
| ramp5-g500 | 322/500 | 0/0 | 0 | 110 | 15794 / 102142 | 111929 | 0 | 3.18 (0.08) | 512 | 193.0 | 1.3 |
| spect-s500 | 50/50 | 208/500 | 0 | 26 | 3062 / 26788 | 38642 | 59405 | 2.70 (0.32) | 457 | 61.7 | 1.0 |
| spect-s1000 | 50/50 | 218/1000 | 0 | 102 | 2952 / 15839 | 37614 | 53250 | 2.74 (0.18) | 747 | 79.5 | 1.1 |
| spect-s2000 | 50/50 | 255/2000 | 0 | 280 | 2315 / 20542 | 20748 | 59516 | 2.58 (0.08) | 1015 | 73.2 | 1.2 |
| spect-s4000 | 50/50 | 200/4000 | 0 | 905 | 4681 / 38803 | 58155 | 46857 | 2.69 (0.20) | 1127 | 46.0 | 1.4 |
| embed-s100 | 50/50 | 0/100 | 0 | 100 | 491 / 751 | 1467 | 0 | 0.47 (0.05) | 82 | 21.4 | 0.4 |
| caps | 87/500 | 225/2000 | 0 | 385 | 9567 / 58732 | 79868 | 59112 | 2.60 (0.09) | 870 | 73.9 | 1.3 |

`plain-gN`: N guests in one room at 20 a second. `rooms4-gN`: four desktop
rooms filled to 125 each, every guest also in one shared chat room.
`cap500-gN`: `/join` admits 500 and answers the rest "the room is full".
`ramp5-gN`: 5 arrivals a second, 120 s to join. `spect-sN`: 50 guests and N
spectators admitted as Viewers. `embed-s100`: 100 spectators on the embed's
path. `caps`: eu1's caps at once, 500 guests and 2000 spectators. A failure is
a guest that had no `Ready` within its join timeout, or whose redemption the
host refused because its challenge expired while it waited. The first sweep
(run 112376, at `957fea1403`, counting the relay process alone) found the same
shape: 300 guests joined there with join p99 55.6 s, 500 and 800 stopped at
275 and 318.

### Where it stops

* **Joining slows with the room, not with the arrival rate.** At 5 arrivals a
  second (`ramp5`) the mean join time of the last tenth of guests is still
  54 s at 300 and 91 s at 500, as at 20 a second. `/join`'s answer alone
  (pre-admission) grows from 77 ms (the last tenth of 50 guests) to 1-6 s
  past 300, and redemptions start failing with "join challenge is expired".
* **Commits stall.** At 100 guests every chat change was committed (97 of
  97); at 300, 10 of 186, and delivered chat p99 is 55 s.
* **It is not CPU.** The host's process tree levels off at about 3 cores of
  the 40 it may use, the busiest thread at under half a core, and the driver
  at about one core. Something serial in the room's path (its commit and
  replay owner, or its storage) is what every guest waits on.

### Storage

Every commit is made durable before it counts. With the host's state on
memory (`TMPDIR=/dev/shm`, the `tmpfs` runs; Perf / room host capacity run
113520 at `4197f2a481`), 100 guests joined with p99 1.9 s instead of 7.9 s
(a disk run in the same job), chat p99 2.2 s instead of 7.2 s, and `/join`
answered in 2-5 ms instead of 30-55 ms while the room was half full. So much
of what a guest waits on at 100 is the runner's disk (an overlay filesystem in
a KVM guest). eu1 keeps its state on a local NVMe volume, which should sync
faster, so these latencies are conservative for it. The ceiling is not storage
alone: on memory, 300 guests slowed the same way (joins of the fifth tenth
took 5 s, 13 of 264 chat changes committed) until the job's 64 MiB `/dev/shm`
filled and the run ended invalid.

### Spectators

On the embed's path, every spectator is refused: the room service certifies
each page's device as the room's one spectator person, and the relay admits
only the devices on a governed room's membership, so the page's subscription
answers `permission-required device not in workspace membership`
(`embed-s100`: 0 of 100). The person's root passes the relay's handshake, so
`tilo_rooms_online_spectators` counts these sessions anyway. A relay fix is in
progress; until it lands eu1 has no working spectators, whatever its
`TILO_ROOMS_MAX_SPECTATORS`.

Admitted as Viewers of their own, spectators join until the room's members
reach the same ceiling (about 250 at once) and the rest time out. So no run
admitted every spectator, and a per-spectator figure could not be derived.
What the runs do bound is memory: it grows with the sessions offered, joined
or not, to 1015 MiB at 2050 and 1127 MiB at 4050.

### Per guest

From the runs that admitted every guest (50 and 100 in one room):

| | per guest |
|---|---|
| memory | 0.98 MiB above an idle host of 40 MiB |
| CPU | 13.7 millicores (73 guests per vCPU) |
| egress | 850 kbit/s, mostly presence: the host forwards every moving cursor to everyone in the room, so a guest's share grows with the room (462 kbit/s each at 50, 656 at 100) |

## Cost per 100 concurrent guests

Rooms of 100 guests, as many rooms per node as its binding resource carries at
the price sheet's 70 % target utilisation (`tools/perf/prices.toml`,
`[room_node.*]`, prices checked 2026-10-14: Hetzner's list prices from
2026-06-15 converted at Hetzner's own USD rate, AWS on-demand us-east-1):

| node | guests per node | bound by | USD per month per 100 concurrent guests | USD per guest-hour |
|---|---|---|---|---|
| Hetzner AX102 (7950X3D, 32 threads, 128 GB, EUR 257.30, 1 Gbit/s unmetered) | 823 | network | 36.73 | 0.00050 |
| Hetzner AX42 (8700GE, 16 threads, 64 GB, EUR 97.30, 1 Gbit/s unmetered) | 818 | CPU | 13.96 | 0.00019 |
| AWS c7a.2xlarge (8 vCPU, 16 GiB, USD 0.41056 an hour, egress USD 0.09/GB) | 409 | CPU | 2,587.38 | 0.03544 |

On AWS, 97 % of that is egress (0.38 GB per guest-hour at USD 0.09); compute
alone is USD 73 per 100 guests a month. On Hetzner, the AX102's 1 Gbit/s port
fills before its cores. Both say the same: at these cursor rates presence
fan-out, not compute, is the cost of a collaborator in a large room. eu1's
marginal cost today is a share of a node it already pays for.

## Sizing eu1

* **Guests:** 500 does not fit. Cap the room at **100** (join p99 under 10 s,
  chat p99 under 6 s, 122 MiB, 1.1 cores here); 50 is the point with
  sub-second joins. Raise it when the room admits more (tracked separately).
* **Memory:** request **1280Mi** (the most any run used, 1127 MiB at 4050
  offered sessions, rounded up), keep the **6Gi** limit: it is more than 1.3
  times every measured peak and than 100 guests plus 2000 admitted spectators
  at a guest's 0.98 MiB each (about 2.1 GiB).
* **CPU:** request **1500m** (100 guests at 13.7 millicores, rounded up); no
  limit, which the room could not use anyway past about 3 cores.
* **Spectators:** keep 2000 until the embed path admits them, then re-run
  `spect-*`, `embed-s100` and `caps` and size from those.
* **Alerts:** guests and spectators above 80 % of their caps
  (`tilo_rooms_max_guests` and `tilo_rooms_max_spectators`, which the room
  service now exports), and the container above 85 % of its memory limit.

The nightly **Perf / scheduled** `hosting-room` job guards the 100-guest
point (`tools/perf/kpi-budgets.toml`, `hosting.room.*.g100`).
