#!/usr/bin/env bash
# Prove that the image oci-publish.sh pushed from OUT can be pulled with no credentials at all,
# for images that jobs pull before anything could log in. For an index, the registry must list
# exactly the images the archive holds.
#
# usage: oci-anonymous.sh REPOSITORY OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
repository=${1:?usage: oci-anonymous.sh REPOSITORY OUT}
out=${2:?}
digest=$(cat "$out/digest")
anonymous=$scratch/anonymous-auth.json
printf '{}' > "$anonymous"
pulled=$(skopeo inspect --raw --authfile "$anonymous" "docker://$repository@$digest")
[[ sha256:$(printf '%s' "$pulled" | sha256sum | cut -d' ' -f1) == "$digest" ]] \
  || die "anonymous pull of $repository@$digest returned other bytes"
expected=$(tar -xOf "$out/image.oci.tar" "blobs/sha256/${digest#sha256:}")
[[ $pulled == "$expected" ]] || die "the registry's $repository@$digest is not the archive's"
printf 'anonymously pullable: %s@%s\n' "$repository" "$digest"
