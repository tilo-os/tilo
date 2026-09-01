# qOS Sync / Host / P2P Model — Design

Status: **design approved (model)**; spec under review. Implementation deferred and
sequenced into sub-projects (below). The transport/host/policy work touches
`shell/`, `fs/`, and `os/`, so it must be coordinated with the compositor/OS track.

## 1. Purpose

Give qOS a multi-device sync + hosting model that feels **smooth and native** — not
bolted-on like current-gen ecosystems (Apple/Samsung) — driven by cryptographic
identity, adaptive to device context, and spanning three deployment tiers with a
**single mechanism**:

1. **P2P (SyncThing-like)** — your devices sync directly when both are online.
2. **Home server** — an always-on host *you* own (self-hosted, provisioned by a
   custom ISO) that keeps workspaces available and relays across networks.
3. **Enterprise cloud** — the same host, operated by us, multi-tenant + billed.

The tiers differ only in **who runs the host and the trust/policy around it**.

## 2. The one mechanism

Everything reduces to: **a certified, always-on replica of your workspaces.**

A **workspace** is *content* (a `WorkspaceTree` CRDT + content blobs), owned by a
**user identity**, addressed by `workspace_id`. Devices are **replicas** of the
workspaces they carry; one (or more) replica can be promoted to **host**. Devices
are *not* workspaces.

Grounding in what exists today:
- Collaboration is already **workspace-centric** and **compositor-orchestrated**
  (`plan/collaboration.md`): a workspace is a gossipsub topic
  `/nirios/ws/<id>/v1`; CRDT sync is incremental automerge (`SyncReq`/`SyncResp`);
  presence + foreign-surface streaming (H.264) + "host-always-wins" input
  arbitration + **signed invite tokens** all exist.
- The FUSE fs already separates **metadata projection** (visible immediately via
  gossip) from **content hydration** (fetched on first read), over **iroh**
  (`fs/`), which does NAT hole-punching + relays.

## 3. §1 — Identity: a user root key that certifies device keys  *(the enabler)*

Today "identity" is a **per-device** ed25519 key (`/var/lib/qos-fs/peer-key`,
`plan/collaboration.md` §2). We add a **user root identity** — an ed25519 keypair
that is *you*, the human.

- Each device keeps its own device key. Your root key signs a **device
  certificate** binding `device_key → user_identity` (with a label, capabilities,
  optional expiry).
- A workspace is owned by a **user identity**, not a device. **Any device
  presenting a valid cert chaining to the owner identity automatically has owner
  permissions** — this is why "a host has the same identity as you" and why you
  "automatically have full permissions" on your own devices/workspaces.
- **Revocation:** a revocation list published in your own identity CRDT lets you
  kill a lost device; peers reject its changes going forward.
- Reuses the existing ed25519 + canonical-signed-token machinery
  (`shell/src/network/invite.rs` already signs/validates tokens this way).

Open detail for the SP1 plan: cert format + chain validation; how the root key is
held/backed-up (the human's key — recovery matters); pairing a new device
(scan/enter an enrollment token minted by an already-trusted device).

## 4. §2 — Workspaces & sync: project all, hydrate on demand, pin to hold

- Your identity carries a small **workspace directory** CRDT — the list of your
  workspaces (`id`, name, member devices, host assignments). It syncs to **all**
  your devices; it is the always-present "core."
- Every device **projects metadata** of all listed workspaces (cheap gossip, as
  the FUSE already does), **hydrates content on open**, and can **pin** a workspace
  to hold it fully offline.
- **Membership** = which devices replicate a workspace; editable from any device.
- Create a workspace on any device; change its device set; assign a host — all
  from the directory.

This layers on the existing automerge-incremental sync + FUSE projection/hydration
— minimal new machinery, maximal discoverability (every device *sees* everything;
bytes arrive on use or on pin).

## 5. §3 — Hosts: certified always-on replicas + relays (plural by design)

A **host** = a device with your cert, marked always-on, that:
- **Pins** its assigned workspaces (persistent replica → sync works when your
  laptop sleeps), and
- **Relays** so your devices reach each other + the host across networks
  (store-and-forward when a peer is offline).

Semantics (per the approved model): a host is **just another collaborator carrying
your identity that validates CRDT changes**. **Normal app logic still runs on your
device and syncs like any collaborator** — the host does not execute your apps.

- **Hosts are plural.** Multiple hosts are just more replicas of the same automerge
  doc (they merge). **Sharding** = different hosts pin different workspaces. A host
  **cluster** = redundancy/availability. The design must not assume a single host.
- Hosts are the preferred **relay/SFU for video conferences** (ties into the
  separate calling-media track: P2P by default, relay via a host when appointed).
- **"P2P by default, host if appointed":** devices try direct first (LAN mdns +
  hole-punching), and fall back to a host relay / store-and-forward.

**Deferred (recorded, not built now):** you may later mark specific expensive
operations to run **only on the host** with the resulting CRDT streamed back. The
model allows it (host is a same-identity collaborator) but the UX is hard — out of
scope for v1.

## 6. §4 — WAN reachability  *(the one genuinely open implementation fork)*

Workspace CRDT sync today is libp2p gossipsub over **LAN only** (mdns; `kad`/relay
is feature-gated off, `plan/collaboration.md` §2). But **iroh** (already the fs
content transport) does NAT hole-punching + relays with stable node-ids.

**Decision to make in the SP2 plan:** either (a) **unify workspace-CRDT transport
onto iroh** (stable node-id per device/host; a host with a public addr or an iroh
relay is reachable anywhere), or (b) **enable libp2p's gated relay + DCUtR**.
Recommendation: **iroh** — the fs already uses it, and it does NAT traversal well;
unifying transports also simplifies the policy engine (§6). This is the single
biggest implementation choice and is resolved in SP2, not here.

## 7. §5 — Adaptive sync policy: the "feels-native" layer

One **policy engine** sits under both transports and decides *when / how eagerly /
in what order* to move changes and bytes, from live signals — so a device
pre-syncs while it is cheap and everything is already there on pickup.

- **Signals in** (sources already present): battery % + charging + **at/near
  charge-limit**, power source; network class (Wi-Fi/cellular, **metered/
  unmetered**, LAN-host-present); device idle vs active + screen on/off; thermal +
  disk pressure; per-workspace (foreground, recently-used, pinned, host-assigned,
  predicted-next). The shell already reads battery/network for the topbar
  (`shell/src/shell/chrome_state.rs`).
- **Decisions out** (per workspace × peer × transport): *rate* (CRDT heartbeat +
  batch-vs-immediate flush), *hydration eagerness* (on-demand → predictive
  prefetch), *priority* (host sync always high + durable; foreground high; cold
  background lazy), *caps* (no large pull on cellular/metered).
- **Rules** (illustrative): charging + at charge-limit + unmetered + idle →
  **aggressive** (predictively hydrate likely-next files, eager CRDT push, warm
  caches); on battery + metered → **frugal** (host + foreground only, defer rest);
  **always** push to host(s) for durability (batched when frugal); LAN peer present
  → prefer direct over relay.
- **One engine, whole stack.** The same policy drives the **CRDT cadence** (today a
  fixed 2 s/5 s heartbeat in the shell network actor) *and* **FUSE hydration/
  prefetch**. `fs/src/smart.rs` is a deliberate stub for exactly this — its docs say
  the `on_open`/`on_readdir` hooks exist "so the FUSE layer has a place to signal
  access patterns that a future policy can turn into background `Hydrator` fetches."
  SP4 generalizes `smart.rs` into a shared policy library that both the fs and the
  shell network actor consult. This unification — not two subsystems with separate
  ad-hoc timers — is what makes sync feel native.

## 8. Deployment tiers = one mechanism

| Tier | Host operated by | Trust / policy | Everything else |
|------|------------------|----------------|-----------------|
| P2P | (no host) | your identity only | identical |
| Home server | you | your cert; self-hosted; ISO-provisioned; headless host mode | identical |
| Enterprise cloud | us | org identities certified to use it; multi-tenant + billing | identical |

## 9. §6 — Provisioning / custom ISO / headless host mode

The imager app builds a **custom ISO** that bakes in your identity's **public key**
+ a **device-enrollment token**, so a new machine (home server) **auto-enrolls as
your certified device on first boot**, joins your workspace directory, and — for a
server — boots **headless in host mode**: the sync/relay daemon (identity, network
actor, fs, policy engine) with **no compositor session**. Remote login is implicit:
your identity is already trusted, so you attach from another device.

## 10. Sub-projects (each gets its own spec → plan → implementation)

1. **SP1 — User identity + device enrollment + revocation.** Root key certifies
   device keys; workspace ownership by identity; enrollment/pairing; revocation
   list. *Gates everything.*
2. **SP2 — WAN transport + host-as-relay.** Resolve the iroh-vs-libp2p fork; stable
   node-ids; host relay + store-and-forward; reach a host over the internet.
3. **SP3 — Workspace directory + projection/pin membership.** The directory CRDT;
   per-device membership; pin; project-all/hydrate-on-open UX end to end.
4. **SP4 — Adaptive sync-policy engine.** Generalize `fs/src/smart.rs`; ingest
   battery/network/idle signals; unify CRDT cadence + FUSE prefetch under one
   policy. The "magic."
5. **SP5 — Provisioning ISO / headless host mode.** ISO bakes identity + enrollment;
   host-mode boot (no session); first-boot auto-enroll.

**Build order:** SP1 → SP2 → SP3 → SP4 → SP5. SP4 can begin against the existing fs
hydration once SP2/SP3 give it multiple peers/hosts/workspaces to prioritize.

## 11. Explicitly out of scope for v1

- Per-operation **compute-offload** to the host (CRDT streamed back) — hard UX,
  deferred; the model permits it.
- **Remote-DE-as-live-compute** (attaching to a *running* remote session): the
  `peer_view` surface-streaming primitive exists, but "run the workspace on the
  host" is the deferred compute-node model. For v1, hosted workspaces are **data**
  (you edit locally; results sync), not a streamed running session.
- The **calling media pipeline** (capture/codec/transport) is a separate track (see
  the historical chat audit, since removed); hosts become its relay/SFU later.

## 12. Grounding index (read before implementing)

- `plan/collaboration.md` — workspace topics, automerge sync, invite tokens,
  host-wins arbitration, surface streaming.
- `plan/security.md` — identity, sandboxing, egress policy.
- `shell/src/network/{invite.rs, actor.rs, protocol.rs, replica.rs}` — live wiring.
- `fs/src/{smart.rs, hydration.rs, namespace.rs, sync.rs}` — projection/hydration +
  the policy seam.
- `shell/src/shell/chrome_state.rs` — existing battery/network signal ingestion.
