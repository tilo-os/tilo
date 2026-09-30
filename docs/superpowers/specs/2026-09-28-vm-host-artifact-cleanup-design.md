# VM host artifact cleanup

## Problem

`tilo-vm gc` collects guest layers and build targets, and `tilo-vm vm rm`
removes a registered VM's owned disk. Installer scenarios also create media
downloads, converted disks, temporary targets, overlays, firmware variables,
and proof files on the host. A failed or interrupted scenario can leave many
gigabytes outside the VM registry. Operators currently have to reconstruct
ownership from filenames, QEMU processes, and qcow2 backing chains.

## Ownership record

Every host-side scenario creates a run directory and atomically writes an
`artifacts.json` record before allocating a large file. Each entry records:

- An absolute canonical path and the file identity after creation, so a later
  replacement at the same path is never removed by an old record.
- The producing run and step, kind (`download`, `medium`, `overlay`, `target`,
  `firmware`, or `evidence`), creation time, logical and allocated bytes.
- Backing-file and parent references, plus the VM or scenario that may still
  consume it.
- Retention state: `scratch`, `checkpoint`, `evidence`, or `external`.

External targets passed with `--target-disk`, physical install sources,
registered VM disks and their base images are **never owned** by a scenario,
even if they reside below its output directory. A failed install may contain
the only useful diagnostic evidence, so failure changes scratch entries to
`review` until the operator explicitly selects that run for cleanup.

## Commands

`tilo-vm scenario gc --dry-run` lists each run and the exact files it could
remove, the estimated allocated bytes, and why other files remain. Applying a
plan requires `tilo-vm scenario gc --apply <plan-id>`; the plan ID hashes the
artifact records, file identities, and backing graph. A changed file or
reference invalidates the plan. `--run <id>` scopes cleanup to one scenario;
there is no filesystem-wide glob or recursive delete of arbitrary paths.

Normal successful runs clean scratch downloads and their temporary target as
soon as the checked proof is durable. A separate `--failed` selection permits
reviewed failed-run scratch cleanup. Checked proofs, source receipts, and
physical readbacks remain evidence until the owning run is explicitly retired.

## Apply-time safety

Before each removal, take the host registry lock and revalidate the plan.
Reconcile QEMU command lines, registered VMs, Windows mounted disk images,
open image handles where available, and every known qcow2 backing edge.
Require the target to be a regular file with the recorded identity inside the
recorded run root, and reject symlinks or reparse points. Remove children
before their backing files. Refuse an unknown or unreadable backing graph.
After deletion, atomically update the record so an interrupted cleanup can
resume without broadening its scope. Never silently clear the Recycle Bin.

## Verification

Exercise successful and interrupted scenarios with qcow2 children, a running
QEMU process, a mounted VHDX, a registered VM sharing a base image, a replaced
pathname, a symlink/reparse point, and a partial deletion. Each must either
retain the referenced file or remove only the exact owned artifact. On Windows,
compare allocated bytes before and after cleanup; logical sizes of sparse raw
files are not a measure of recovered space.
