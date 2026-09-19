# Small assembly changes

The late trust section previously copied seven small files in four separate
rootfs instructions. It now copies them into one temporary directory and places
them at their original destinations in the existing trust-install instruction.
The per-run public bundle remains separate. Native applications, Windows
runtimes, browser and all preceding rootfs instructions remain unchanged.

This removes **three full-rootfs COPY snapshots and their intermediate cache
publications** on a miss in that section. It does not remove the later kernel,
dracut, validation or retention work. No end-to-end speedup is claimed without a
subsequent measured assembly. Registry cache export policy is unchanged.

A scratch overlay copied directly onto `/` would also overwrite metadata on
existing parent directories. Staging into an isolated directory and copying the
individual files preserves those directories. The preparation uses the existing
trust-install RUN, so it does not add another large-image layer.

## Cheap equivalence check

On a Linux host with local rootful Podman, use an **already cached Fedora image**:

```sh
python3 os/tests/trust_input_copy_equivalence.py --tools-image IMAGE_ID
```

The test extracts the changed instructions from the production Containerfile and
compares them against the previous copy layout on the same cached image. It
checks bytes, file types, modes and numeric ownership, with nondefault existing
directory modes, an existing policy file, unrelated files and temporary-directory
removal. No registry access, OS compilation or cache publication is performed.
Only the test's two temporary image IDs and temporary context are removed.

This is filesystem equivalence before public-trust installation. Existing trust
validation still owns cryptographic/public-key policy. This change does not
authorize reusing a native receipt under a new source revision: the controller's
normal source compatibility and timestamp rules still apply.

## Next storage improvement: a bounded reusable assembly worker

Proposed deployment, **not enabled by this change**:

1. Use one reusable ARM64 raw-assembly worker with a dedicated **160 GiB** block
   volume for its Podman graphroot. Retain it between jobs. Keep each job's
   checkout, evidence, registry authentication and signing material outside that
   volume. A volume alone is insufficient if the current worker teardown still
   deletes the graphroot or the next Kata guest cannot attach the same device.
2. Allow one build at a time against that graphroot, with an exclusive lease
   owned by the worker. Persist the Podman/storage-driver version and filesystem
   identity with the cache; refuse an incompatible reuse. Never attach or prune
   it while another guest or build owns it. Separate ARM64 and x86-64 workers.
3. Admit a job only when its measured compressed/unpacked inputs, scratch budget
   and a **20 GiB reserve** fit. Aim for cached occupancy below **100 GiB** after
   a completed job. These are proposed bounds, not a claim that every OS payload
   fits; reject oversize admission rather than filling the volume.
4. Between jobs, first delete only recorded stopped build containers. Then evict
   least-recently-used, unreferenced cache images older than **seven days**, using
   an explicit cache-owned ID inventory. Pin current input digests and the last
   successful raw rootfs; do not use a global prune. If the occupancy target
   cannot be reached without removing a pin, reject admission and report it.
5. Keep registry recovery checkpoints and full artifact verification. Local
   storage is a performance cache, never provenance. Measure the same tiny edit
   twice on the retained worker before changing export frequency: step time,
   cache hits, CPU, disk bytes/latency and network bytes. A local hit should avoid
   download/unpack; an edited COPY can still scan and publish the large rootfs.

Roll this out only after the current install image is complete. It needs an
infra change and an attach/restart acceptance check, not another application build.
