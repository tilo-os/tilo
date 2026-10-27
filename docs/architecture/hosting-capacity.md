# Hosting capacity, load and retained resources

What a Tilo host — the host program on a person's own machine, on a relay, or
on tilo.coffee — promises while several people use it at once, which limits
it enforces on the way, and what it keeps afterwards. [`hosting.md`](hosting.md)
says what a host is; this page says how much of one there is and what it costs.

The policy is here and the numbers are derived from the code, so changing a
limit means changing both in one commit. This page states what a host promises
and how it is checked; it does not measure a machine. One machine's numbers
come from `perf-room-capacity` and `docs/benchmarks/room-host-capacity-2026-10.md`,
and they go stale with the tree.

## A host is one control plane, exactly as many as its node runs

A host is one process tree serving every workspace it holds. That is the
design ([`hosting.md`](hosting.md), "One host serves many workspaces"): no
VM, container or process per workspace. So the capacity a host has is the
capacity of that one process tree, split between the workspaces it holds —
and the first thing a load question asks is which of its paths are shared.

The shared paths in the code today:

| Path | Shared by | Bounded by |
|---|---|---|
| Room owner, commit and history replay | every workspace the relay governs | implemented, but the bound is per room: nothing between rooms. Measured as the room ceiling, `docs/benchmarks/room-host-capacity-2026-10.md` |
| SDK command and event queues between an app and its transport | one app process | separate retained byte budgets for documents and control traffic, so one large document neither starves nor fails an action (`crates/sdk/ui-core/src/collaboration/broker.rs`) |
| Fetch proxy: requests and bytes | the whole host | per device, per destination together, and in flight together (`crates/platform/tilo-net/src/proxy/host.rs`, `RateLimits`) |
| Fetch proxy: response bytes | one visitor and one room | rolling budgets (`Budget::visitor_bytes`, `Budget::room_bytes`) |
| Room joins | one person, one address | join windows and cooldowns (`crates/hosting/tilo-room-service/src/limits.rs`) |
| Record changes and content bytes a person sends | one person, whole host | token buckets per person (`crates/hosting/tilo-relay/src/member_limits.rs`, `MemberLimitsConfig`), opted in by the operator |
| Environment sessions | the whole host and each member | `EnvironmentsConfig`: `capacity` sessions at once, `member_sessions` each, plus disk, memory, CPU and process limits enforced per member with cgroups |
| Compute assignments | the whole node | the worker's advertised `assignment_capacity` (`placement.rs`, `ServerAdvertisement::compute_ineligibility`) |
| Model summarisation calls | the whole host | `SummaryLimits`: calls per rolling hour and the least interval between two for one person |

Every one of those bounds a *tenant* — a person, a device, a room — against
the host or against itself. The gap is the room owner, which is what a busy
room actually waits on. The write-up measured one room: 50 guests served well,
100 acceptably (join p99 9.9 s, chat p99 5.5 s), and past about 250 members it
stops admitting while using about 3 of its 40 cores. Its `rooms4-*` runs, four
rooms filled to 125 each, did worse rather than better, which is what a shared
serial path looks like — but they were never written up, and this page does
not treat them as a measured figure. Until that path holds a per-room share, a
host's declared capacity is what one room of its size carries, not the sum of
what several would.

## Declared capacity and the service level

A public room host is declared for **100 guests in one room**. Past that it
admits no more: `/join` answers that the room is full, with a wait, and the
host keeps serving the hundred it has. Admission caps larger than that
(`TILO_ROOMS_MAX_GUESTS`, up to 10000 for a persistent room) are a backstop
against a runaway admission, not a capacity claim.

The service level at that capacity, which is what the probe below holds:

| | at 50 guests | at 100 guests, the declared capacity |
|---|---|---|
| join, p99 | 1 s | 10 s |
| chat delivery, p99 | 2 s | 6 s |

The workload is the one the write-up measured, and the one `tilo-room-load
bench` drives: guests arriving one after another, a cursor published at 30 Hz
while it moves and moving 30 % of the time, and a 300-byte chat change every
60 s on average. A join is bounded because the first thing a newcomer meets
is the room's replay; chat delivery because a conversation that takes longer
than that is one the host made slow.

Those are promises about a machine nobody shares. A host on a node with other
work does not meet them, so the probe measures the workload under a degraded
host — 70 % of the host's busiest second held back, the link to it at 3
Mbit/s — on a node whose whole quota is known. `perf-room-degraded` (t.toml,
`tools/ci/perf-room-degraded.sh`, `tools/perf/room_capacity.py degraded`) runs
it in the performance run, beside the reference run at 50 guests with nothing
else on the machine. A run at the declared capacity that misses either number,
a room past its cap that stops answering in time, a failed session, or a
degradation the host was too idle to feel all fail the check — the last one
because a measurement that cannot observe its subject would otherwise pass.

### Procedure: qualifying a node

1. Run `t dispatch perf-room-degraded --ref <branch>` and read `t outputs
   perf-room-degraded`. The machine description it prints says which node and
   which cgroup quota the numbers came from.
2. If the probe passes, the node carries 100 guests. Its binding resource (the
   first of CPU, memory and port to run out at 70 % utilisation,
   `tools/perf/prices.toml`) says how many such rooms it carries side by side,
   from `perf-room-capacity`'s per-guest figures — and that number is what the
   room owner path above still has to earn.
3. Set `TILO_ROOMS_MAX_GUESTS` to the declared capacity. Larger is a backstop,
   smaller is a host that refuses guests it could serve.
4. Re-run the probe after changing anything the workload's cost depends on —
   `MemberLimitsConfig` (the per-person change and content budgets), the
   fetch-proxy `RateLimits` and `Budget`s, `EnvironmentsConfig`'s `capacity`,
   or a relay's own advertised `assignment_capacity`.

## Quota and capacity refusals are actionable and cheap

The host's refusals say which bound was reached and for how long, and every one
of them is produced before any paid work starts, so a refused request never
launches twice:

- **Admission** answers that the room is full with the wait until a seat frees
  (`FULL`, 60 s in `tilo-room-load`'s host as the room service answers it), and
  that too many people are watching past the spectator cap. Neither starts a
  relay back up nor charges anything: the seats are the relay's own count.
- **Environment sessions** are refused against `capacity` and `member_sessions`
  before a supervisor starts, so a refused session costs no cgroup, no disk and
  no uid.
- **Compute** refuses an assignment at `AtCapacity` from the worker's own
  advertised count during eligibility, which is the same rule placement uses,
  so a job is never named onto a worker that would then refuse it.
- **A person's budgets** (`MemberLimits`) refuse with `TOO_MANY_CHANGES` and
  `TOO_MUCH_CONTENT` and refill; they never disconnect, so a script that
  overruns is throttled while the person beside it notices nothing.
- **The fetch proxy** bounds requests and bytes per device, per destination
  and in flight, and response bytes per visitor and per room over a rolling
  window, so one busy room cannot spend a host's transfer allowance on its own.

The number to watch is not only the count the refusal compares against but the
wait it names: a host whose joins are refused at 100 guests is at its declared
capacity, which is a fact to report rather than a fault to fix. t's
`perf-hosting-room` check guards the 100-guest point
(`tools/perf/kpi-budgets.toml`, `hosting.room.*.g100`), and the room service
exports `tilo_rooms_max_guests` and `tilo_rooms_max_spectators` for the alert
below.

## Retained resources and what they cost

What a host keeps after the people have gone, and what removes it:

| Retained | Why it is retained | Removed by |
|---|---|---|
| A room's relay, after its last guest leaves | a relay governing a room keeps its port pair, its resolved content and its history, so a returning guest is fast on it | the room's time to live (`TILO_ROOMS_TTL_SECS`, 1800 s by default), after which the room service tears the room down |
| A room's state on the host's volume | the history it keeps for the room's members | the room's retention and compaction (`retention.rs`, `compaction`), and the teardown, which removes its directory |
| The guest ledger | a key per person and an address hash per join, kept across a restart | self-pruning: joins are kept 7 days, cooldowns and removed people 24 hours (`tilo-room-service/src/guests.rs`) |
| A member's environment | disk to `member_disk_bytes` (10 GiB by default) and memory to `member_memory_bytes`, both per member | `idle_timeout_secs` (30 minutes) ends a session that went quiet; its disk is bounded per member while it lives |
| A compute assignment | one of the worker's advertised slots | the assignment ending |
| A fetch-proxy budget | memory per visitor and per room, for the window it lasts | the window's own end (`Budget::window`, 24 hours); nothing is written |
| A node's build and content caches | the volume, and the egress a repeated fetch saves | the cache's own eviction policy |

The guest ledger and the fetch-proxy budgets forget themselves; the rest are
bounded by a policy number, and each of those numbers is in the table.

**No retained resource is inventoried per room today.** A host's operator can
see the rooms it holds and their volumes on disk, but nothing enumerates what a
removed room or a revoked member left behind and what it is costing. That is the
gap this page leaves open, and it is the one that matters for a bill: a host
does not lie about what it is serving, it forgets what it stopped serving.

So the inventory is a monthly review, not a dashboard someone reads. It
establishes what the host holds and what it kept for nobody; each step is a
check against the host, not an assumption about it:

1. **Rooms:** every room the host holds, against the rooms that have been used
   in the last 30 days. A room with no guest in 30 days is a room to tear down
   or to move, and its state is on disk meanwhile.
2. **Volume:** the sum of room state directories against the volume's capacity,
   and the growth of that sum over the month. Growth that outruns guests joined
   is history kept for people who are not coming back.
3. **Members and revocation:** every member removed in the month, and whether
   their environment was torn down, their compute slots released and their keys
   rotated out (rotation is `hosting.md`, "Governance").
4. **A node's network:** the egress the month charged, against the egress the
   measured per-guest figure predicts for its declared capacity. A host above
   its prediction is forwarding more than the workload it declared, which is
   usually presence in a room larger than declared.
5. **The thresholds these feed** are the two counters the room service exports
   (guests and spectators above 80 % of their caps, the container above 85 % of
   its memory limit — `docs/benchmarks/room-host-capacity-2026-10.md`,
   "Sizing eu1"), set as alerts that page a person, and reviewed in this monthly
   pass. An alert naming a number nobody reviews is not a threshold.

Whatever they would need beyond this repository — a cloud account's budget and
cost alarms over the nodes Tilo runs — is configured where those accounts are,
not here.

## One zone, and what recovery from losing it means

Every node Tilo runs today is in one declared subnet and one availability zone.
That is a fact about the deployment, not a property of the software, and the
manual says so rather than implying anything about failover: **there is no
automatic failover, and losing the zone loses every node in it.**

What that costs, stated as recovery objectives rather than hoped for:

- **RPO, content:** zero for anything published and acknowledged — a change is
  acknowledged only after the room host made it durable, and every other
  member's device holds its own copy of what it synced. A workspace whose host
  is gone is recoverable from any member's device that holds its history.
- **RPO, the zone's own storage:** whatever the node's volume snapshot contains,
  and no more. A node with no volume snapshot loses its state with the zone.
- **RTO:** the time to stand a host up elsewhere and have its members rejoin.
  That is a manual procedure today: the host program starts on another node,
  the workspace's members add it, and their state is exchanged again. Nothing
  in the tree elects a replacement host or moves a membership, and this page
  does not claim a number for it until one is measured by running it.

Dependencies of a zone incident, in the order they decide the outcome:

1. **Identity and sign-in.** A member reaches a workspace with their own keys,
   so recovery does not need the zone — as long as the material a member needs
   to sign in is on their own device or in their own recovery code.
2. **Naming and links.** Invite links and room origins name the host that is
   gone. `catalogue.rs` mints labels under the operator's `web_entry`; a
   replacement host is a new origin, and members re-add it rather than resolve a
   new address for the old one.
3. **Relay reachability.** Peers find each other over Iroh and fall back to a
   relay only when a direct path fails; with the zone gone, members on the same
   network still reach each other directly, which is what makes a local
   replacement host usable before any infrastructure exists.
4. **Backups.** The zone's storage snapshot is what restores a host's state
   without its members; without one the recovery starts from members' copies.

The test of this section is the recovery exercise, and it is not: running it
means taking a host away. What is checkable in the tree today is the first
dependency and the last — that a workspace's state is on its members' devices
and that a backup restores — and both are verified by the storage and sync
tests, not by this page.

## What this page does not decide

- **More than one zone.** Replicating a host across zones is a separate feature
  decision: it needs placement that names a second host, a governance step to
  admit it, and a way for a member to be routed to whichever one is up. None of
  that exists, and the single-zone statement above is what is true until it does.
- **The room owner's fairness.** The measured ceiling is one room's. What the
  per-room share should be — a queue per room, a share of replay slots, or a
  scheduler change — is a design for whoever works on that path; this page only
  states that today there is none.
- **Owners and rollout.** Who runs the monthly review, and which alerts are
  wired to which channel, is an operational arrangement outside the tree.
