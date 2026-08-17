#!/usr/bin/env bash
# Does this machine's container storage preserve a layer's bytes across a layer
# boundary?
#
#   ./os/tests/layer_capture.sh            # the store podman would use
#   PODMAN_ARGS='--root … --runroot …' ./os/tests/layer_capture.sh
#
# Why this is a test and not a footnote. An overlay store that has a *mount
# program* (fuse-overlayfs) cannot use the kernel's overlayfs metadata to work
# out what a layer changed, so containers/storage falls back to the "naive"
# diff: it walks the whole tree and re-reads every file whose stat differs. On
# this repository's reference machine that path does not reproduce a large file
# that a transaction rewrote in place. Measured here, replaying real dnf steps
# onto a real parent layer:
#
#   fuse-overlayfs   rpmdb.sqlite came back from the layer boundary with the
#                    same length and the same allocated block count but
#                    different content — a mix of page images from before and
#                    after the transaction. `PRAGMA integrity_check` then
#                    reports invalid page numbers and btreeInitPage errors, and
#                    the next dnf transaction fails against the damaged indexes.
#   native overlay   every boundary byte-identical, in every run.
#
# So the property this checks is not cosmetic and not a preference: with a mount
# program in the path, an OS image build is a dice roll per layer. `podman info`
# reports it directly, which is why the check is cheap enough to be a pre-flight.
#
# A check that cannot observe its subject FAILS. If `podman info` cannot be read
# or does not report the field, that is a failure, not a pass: an unreadable
# answer is exactly the case where the build would proceed on an unknown stack.
set -uo pipefail

PODMAN_ARGS="${PODMAN_ARGS:-}"
fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

# shellcheck disable=SC2086
info="$(podman ${PODMAN_ARGS} info --format \
  '{{ .Store.GraphDriverName }} {{ index .Store.GraphStatus "Native Overlay Diff" }} {{ index .Store.GraphStatus "Backing Filesystem" }}' 2>&1)"
rc=$?
read -r driver native backing <<<"$info"

if [ $rc -ne 0 ] || [ -z "${driver:-}" ]; then
  bad "could not read the graph driver out of \`podman info\`, so whether this
      store tears a layer is unknown. Refusing to guess. podman said:
$(printf '%s\n' "$info" | head -5 | sed 's/^/      /')"
  exit 1
fi
ok "graph driver: $driver (backing filesystem $backing)"

case "$driver" in
  overlay)
    case "${native:-}" in
      true)
        ok "layer changesets come from overlayfs metadata (Native Overlay Diff: true)"
        ;;
      false)
        bad "this store has \"Native Overlay Diff: false\", so every layer's
      changeset is produced by walking and re-reading the tree through a mount
      program (fuse-overlayfs). That is the path that returns a large,
      in-place-rewritten file with mixed page images, which is how the rpm
      database arrives corrupt in the next dnf step. It is also ~15x slower per
      layer commit on this tree.

      The flag is sticky per store: containers/storage records
      GRAPHROOT/overlay/.has-mount-program once it finds fuse-overlayfs
      xattrs on the layers, and from then on that store must keep using
      fuse-overlayfs — the opaque markers in those layers are in xattrs the
      kernel driver does not read. So the store cannot be converted in place.

      Build in a store that has never seen fuse-overlayfs instead. os/dev builds
      do this automatically (QOS_BUILD_STORE); to do it by hand:

          podman --root NEW/graph --runroot NEW/run \\
                 --storage-opt overlay.mountopt=nodev build …

      Note the mountopt: the system storage.conf here asks for metacopy=on,
      which rootless overlay cannot have, and requesting it is enough to send
      the probe back to fuse-overlayfs."
        ;;
      *)
        bad "\`podman info\` did not report \"Native Overlay Diff\" for the overlay
      driver (got '${native:-}'). Whether this store tears a layer is therefore
      unknown, which is a failure and not a pass."
        ;;
    esac
    ;;
  btrfs|zfs|vfs)
    # These compute a layer from snapshots or a full copy; there is no tree walk
    # to re-read a file inconsistently. Slow, in the vfs case, but sound.
    ok "$driver does not derive layers by re-reading the tree"
    ;;
  *)
    bad "unrecognised graph driver '$driver'. Whether it preserves a layer's
      bytes has not been established here, and an OS image is not the place to
      find out."
    ;;
esac

exit "$fail"
