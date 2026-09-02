# Device sync, remote control and serving mesh

qOS treats a person's devices as certified peers in the same collaboration
system. Device sync is not a separate app service: the same signed workspace
operations used with another person are replicated between devices belonging
to one identity. Same identity changes the authorization result, not the
signature, revocation or record-validation path.

The shell also has a private, user-root-derived continuation lane for the
device-specific part of “continue on another device.” Its bounded checkpoint
contains safe app launch intents, logical file references and per-device focus;
it excludes local paths, credentials, compositor surface ids and file bytes.
An authenticated same-identity peer may merge the checkpoint and discover a
dormant workspace, but receipt never launches apps or steals focus. The
always-on relay retains the latest aggregate checkpoint so sleeping devices can
catch up later without exposing personal session state to collaborators.

## One replication substrate

`qos-relay::mesh::ReplicationIntent` maps a device's residency choice onto the
existing data planes:

- workspace layout, app records, ebook reading state and media-library records
  use signed collaboration operations;
- file names, metadata, versions and ACLs use collaborative filesystem records;
- file, book and media bytes use the filesystem's content-addressed blob
  hydration and deduplication.

An app may register an `AppRecords` resource family, but may not create its own
account, pairing, sync status or conflict protocol. Selecting ebook or media
replication automatically includes the file tree and blobs on which it depends.
An always-on *composite* server that starts every referenced data plane may
default to `FullWorkspace`; an interactive device defaults to on-demand
hydration. The standalone `qos-relay` deployment is intentionally relay-only:
it defaults to `WorkspaceOnly` and cannot advertise content residency until an
authenticated filesystem/blob host is running.

## Device roles and authority

A device is interactive, always-on, or hybrid. Runtime service advertisements
are intersected with the immutable capabilities in its root-signed device
certificate. In particular, only `can_act_as_host` devices serve, and retaining
all workspace content additionally requires `can_read_all_workspaces`.
Revoked, expired, foreign-identity or wrongly scoped devices remain rejected by
the existing authentication and operation validators.

An always-on device can pin workspace state and store-and-forward collaboration
traffic. It can retain content, relay a remote-control connection, or advertise
app/compute capacity only when the corresponding service has actually started
and passed its own health checks; a certificate authorizes that service but does
not create it. Relay permission is not control permission:
the target compositor still creates the consented share, gates its surfaces and
filters remote input. Direct control and relayed control therefore reach the
same target-side authorization path.

## Multiple servers

`mesh::place` filters offline, saturated, uncertified/ineligible, wrong-workspace
and wrong-app servers, then uses stable weighted rendezvous placement. The same
request stays on the same server while it remains healthy; different clients,
workspaces and apps distribute naturally; requesting multiple replicas yields
distinct hosts. Capacity, latency and operator priority are bounded routing
signals rather than authorization.

The first workloads are workspace replication, content residency, inbound
collaboration/chat relay and remote-control relay. App serving and sandboxed
compute use the same placement model. This lays the distributed-computing
boundary without pretending the relay daemon already executes arbitrary jobs.

## Integration seams

- Shell/device settings write device role and per-workspace replication intent.
- `qos-relay` exposes its effective `DeviceMeshProfile` through `RelayHandle`.
- The workspace runtime consumes the workspace subscription; filesystem and app
  runtimes consume the resolved resource set.
- Reachability discovery produces `ServerAdvertisement` values only after the
  peer's device certificate and same-identity/workspace relationship verify.
- The remote-control client first tries the target directly, then requests a
  `RemoteControl` placement and tunnels the existing encrypted session through
  the selected relay.
- An app/compute worker will consume `App`/`Compute` placements and must retain
  qpkg sandboxing, input/output capability checks and signed result attribution.

No new wire message was added to `qos-relay/1`: adding an enum variant to the
postcard protocol would make old peers reject the stream. Service discovery and
job execution need a versioned, negotiated protocol extension before rollout.
