#!/usr/bin/env bash
# Run a command inside the image an OCI archive holds, the way a job would run in it: an image
# test between building and publishing, with no daemon. Exits with the command's status.
#
# usage: oci-run.sh ARCHIVE COMMAND [ARG]...
#
# Runs in the buildah image, like oci-build.sh. The command runs with the image's user,
# environment and working directory.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
archive=${1:?usage: oci-run.sh ARCHIVE COMMAND [ARG]...}
shift
(($#)) || die 'no command'
use_buildah buildah-run
container=$(b from --quiet "oci-archive:$archive")
trap 'b rm "$container" >/dev/null 2>&1 || true' EXIT
b run "$container" -- "$@"
