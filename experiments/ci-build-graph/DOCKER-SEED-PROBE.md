# Isolated trusted Docker seed experiment

The live on-premises cluster exposes `zfs-localpv-block-cache` (OpenEBS ZFS
driver 2.11.0, Block PVCs, WaitForFirstConsumer) and `zfs-localpv-snap` with a
working CSI snapshot controller. An existing Bound sandbox Block PVC has a
VolumeSnapshot data source, demonstrating that block restore is already used.
ZFS PV node affinity makes this a **per-node** mechanism.

## Observed result (2026-09-18 UTC / September 19 local)

The isolated experiment succeeded on `talos-2a014f9307124fd2`:

- Initial trusted builder pull: **222 seconds**.
- Fresh snapshot clone: local inspection found the exact expected image before
  any registry pull; the subsequent digest pull took **1 second**, reporting
  "Image is up to date" with no layer download/extraction.
- Clone Pod creation to successful termination: **18 seconds**, including disk
  restore, Kata/container startup, packages, Docker startup and clean shutdown.
  This is one observation, not a fleet percentile or production CI measurement.
- Both Jobs exited zero after `CLEAN_DAEMON_STOP_AND_UNMOUNT`. Both Jobs/Pods and
  the disposable clone PVC were removed. The unmounted trusted seed PVC
  `ci-trusted-builder-seed-probe` and ready snapshot
  `ci-trusted-builder-seed-snapshot` remain in `agent-sandboxes`; restore size is
  50 GiB. Snapshot UID: `d71d6090-a9eb-4c0b-8fac-610c6e1d4d35`.

Receipts are in `results/docker-seed-20260919/`. They identify the immutable
builder/DinD digests, Pod UIDs, exact image IDs, completion times and snapshot
data source. No production runner was switched to this seed by the experiment.

`docker_seed_probe.py` renders dedicated resources, never applies them. The
seed and clone each request 2 CPUs / 4 GiB plus Kata's 0.5 CPU / 6 GiB overhead,
with separate retained 50 GiB Block PVCs. They use the same pinned DinD image,
overlay2 driver and trusted builder digest. They have no Forgejo runner identity,
job payload or Kubernetes service-account token. Registry credentials are a
read-only Kubernetes Secret volume outside the persisted Docker directory.

The seed refuses any already-formatted source disk. It pulls only the configured
trusted builder, checks there are no containers or volumes, terminates and waits
for Docker, syncs and unmounts before reporting success. Snapshot creation must
follow successful completion and deletion of its Pod so no writer remains.
The clone refuses an unformatted disk and requires both the seed marker and a
successful local image inspection **before** its timed registry pull.

## Candidate broker integration

Keep this opt-in until the experiment passes. Configuration should map
`(authorized repository, architecture, node, builder digest, DinD digest,
storage driver)` to an immutable, ready VolumeSnapshot and restore size.
The broker must validate namespace, snapshot UID/readiness, same source node,
Block mode, and requested disk size at least the snapshot restore size. Add the
snapshot as `dataSource` to the existing per-job ephemeral PVC claim template.
Keep a separate writable clone per VM and pin placement to that snapshot's node.

Seeded runners must be restricted to repositories authorized to read the private
builder. Instance-wide generic runners cannot receive private seed contents.
For the current broker patch, require `not warm`, `job.repo_id == 23`, enabled
repository-scoped runner token configuration, `amd64`, and the snapshot's exact
node. Anonymous warm pools remain unseeded until repository-scoped warm-pool
registration and accounting are implemented.
Never promote a post-job disk back to a seed. Seed construction is a trusted
maintenance operation with no user jobs, followed by daemon shutdown and Pod
removal; changing builder or DinD digests creates a new seed generation.
Startup should fail on missing/mismatched expected seed markers instead of
formatting over an intended restored seed. The unseeded path remains available.

This can avoid both the layer transfer and repeated extraction for new VMs.
A registry mirror alone still leaves each isolated daemon downloading and
extracting the layers. No production runner disk is modified by this probe.
