#!/usr/bin/env bash
# Prove that the image oci-publish.sh pushed from OUT can be pulled with no credentials at all,
# for images that jobs pull before anything could log in. For an index, the registry must list
# exactly the images the archive holds.
#
# With TILO_PUBLISH_DRY_RUN=DIR it reads the OCI layout oci-publish.sh's dry run wrote instead.
#
# usage: oci-anonymous.sh REPOSITORY OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
repository=${1:?usage: oci-anonymous.sh REPOSITORY OUT}
out=${2:?}
digest=$(cat "$out/digest")
anonymous=$scratch/anonymous-auth.json
printf '{}' > "$anonymous"
pulled=$scratch/anonymous-manifest.json
if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
  cp "$TILO_PUBLISH_DRY_RUN/$repository/blobs/sha256/${digest#sha256:}" "$pulled"
else
  skopeo inspect --raw --authfile "$anonymous" "docker://$repository@$digest" > "$pulled"
fi
[[ sha256:$(sha256sum < "$pulled" | cut -d' ' -f1) == "$digest" ]] \
  || die "anonymous pull of $repository@$digest returned other bytes"
# By hash, not cmp: the skopeo image has no diffutils.
[[ $(tar -xOf "$out/image.oci.tar" "blobs/sha256/${digest#sha256:}" | sha256sum) == "$(sha256sum < "$pulled")" ]] \
  || die "the registry's $repository@$digest is not the archive's"
printf 'anonymously pullable: %s@%s\n' "$repository" "$digest"
