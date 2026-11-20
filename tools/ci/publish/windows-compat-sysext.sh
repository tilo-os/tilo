#!/usr/bin/env bash
# Publish the Windows compatibility system extension os-windows-compat-ARCH built
# (os/usr/share/tilo/system-extensions/README.md, "Prebuilt sysext images"), where
# extension-manifests.sh resolve looks for it:
#
#   windows-compat-sysext.sh publish ARCH BUILT
#
# BUILT: os-windows-compat-ARCH's output, the windows-compat-sysext stage as an OCI layout in
# BUILT/windows-compat-sysext/oci, holding tilo-windows-compat.raw and the descriptor its
# packer printed (os/dev/build-windows-compat-sysext.sh). It uploads, to the generic packages:
#
#   tilo-windows-compat-image/ARCH-<sha256>/tilo-windows-compat.raw  the image, by its content
#   tilo-windows-compat-image/ARCH-<commit>/descriptor.json          what the dev image of
#                                                                    <commit> installs
#
# The image goes first, so a descriptor never names a file that is not there; both are
# immutable, and publishing again finds them there and leaves them. The commit is T_COMMIT (main's
# dev images name theirs: os-dev-publish.sh), or T_INPUT_REVISION, a commit whose dev image is
# already published and whose payload this tree builds unchanged: every file the payload's
# build reads is the same at both, which this checks before publishing for it.
set -euo pipefail
here=$(dirname -- "${BASH_SOURCE[0]}")
. "$here/lib.sh"
cd "$repo_root"
usage='usage: windows-compat-sysext.sh publish ARCH BUILT'
[[ ${1:-} == publish ]] || die "$usage"
arch=${2:?$usage}
built=${3:?$usage}/windows-compat-sysext/oci
case $arch in x86_64 | aarch64) ;; *) die "no Windows compatibility extension for $arch" ;; esac
[[ $(channel) == dev ]] || die "the Windows compatibility extension publishes for main's dev images only; this run is for $T_REF"
require_commit
[[ -n ${TILO_PACKAGES_TOKEN:-} ]] || die 'TILO_PACKAGES_TOKEN is required: the check needs secrets = ["TILO_PACKAGES_TOKEN"]'
[[ -f $built/index.json ]] || die "$built holds no OCI layout: the check needs os-windows-compat-$arch"

revision=${T_INPUT_REVISION:-$T_COMMIT}
[[ $revision =~ ^[0-9a-f]{40}$ ]] || die "revision must be a full commit, not $revision"
if [[ $revision != "$T_COMMIT" ]]; then
  git cat-file -e "$revision^{commit}" 2>/dev/null || git fetch --quiet origin "$revision" \
    || die "could not fetch $revision"
  git merge-base --is-ancestor "$revision" "$T_COMMIT" || die "$revision is not on main before $T_COMMIT"
  mapfile -t reads < <(python3 -I tools/ci/os/pieces.py inputs windows-compat --given '')
  # What decides the payload besides the files its stages copy: the stages themselves, the
  # pinned downloads (Proton, the Steam runtime, UMU) and the base its tools work in.
  reads+=(os/image/helpers os/image/args.Containerfile os/image/layers.toml
    os/external-assets/github-assets.tsv os/base-snapshot.env)
  paths=()
  for read in "${reads[@]}"; do paths+=("${read%%/\*\*}"); done
  if ! git diff --quiet "$revision" "$T_COMMIT" -- "${paths[@]}"; then
    git diff --stat "$revision" "$T_COMMIT" -- "${paths[@]}" >&2
    die "the payload's build changed since $revision: it publishes only for a dev image built with the same inputs"
  fi
fi

work=$scratch/windows-compat-sysext
rm -rf "$work"
mkdir -p "$work"
python3 -I - "$built" "$work" <<'PY'
import json, os, sys, tarfile
layout, out = sys.argv[1:]
def blob(digest):
    return os.path.join(layout, "blobs", *digest.split(":", 1))
[entry] = json.load(open(os.path.join(layout, "index.json")))["manifests"]
manifest = json.load(open(blob(entry["digest"])))
wanted = {"descriptor.json", "tilo-windows-compat.raw"}
for layer in manifest["layers"]:
    with tarfile.open(blob(layer["digest"]), "r:*") as tar:
        for member in tar:
            name = member.name.removeprefix("./")
            if name in wanted and member.isfile():
                with tar.extractfile(member) as src, open(os.path.join(out, name), "wb") as dst:
                    while chunk := src.read(1 << 20):
                        dst.write(chunk)
                wanted.discard(name)
if wanted:
    sys.exit(f"the windows-compat-sysext stage holds no {', '.join(sorted(wanted))}")
PY
descriptor=$work/descriptor.json
raw=$work/tilo-windows-compat.raw
read -r size sha256 file < <(python3 -I -c '
import json, sys
d = json.load(open(sys.argv[1]))
assert d["schema"] == 1 and d["part"] == "tilo-windows-compat" and d["file"] == "tilo-windows-compat.raw", d
print(d["size"], d["sha256"], d["file"])' "$descriptor") || die "$descriptor is not the packer's descriptor"
[[ $(stat -c %s "$raw") == "$size" ]] || die "$file is not the $size bytes its descriptor pins"
[[ $(sha256sum "$raw" | cut -d' ' -f1) == "$sha256" ]] || die "$file does not have the SHA-256 its descriptor pins"

# Merging puts the image's directories in place of /usr's, so one without SELinux
# labels leaves /usr, /usr/lib and /usr/share unlabelled: every new login hangs and
# the next start stops in systemd-tmpfiles-setup. The build checks its image; this
# re-reads the bytes about to be offered and refuses them instead of trusting it.
command -v debugfs >/dev/null \
  || dnf install -y --setopt=install_weak_deps=False e2fsprogs >/dev/null
python3 -I "$repo_root/os/dev/sysext-image.py" check "$raw" \
  || die "$file would change /usr when merged (os/dev/sysext-image.py names what)"

package=https://code.tilo.so/api/packages/tilo/generic/tilo-windows-compat-image
export TILO_PACKAGE_TOKEN=$TILO_PACKAGES_TOKEN
uploads=$work/uploads.tsv
# The image is named by its content: one already there is these bytes, and sending 2 GB
# again only to be told so is skipped.
status=$(curl --silent --show-error --retry 3 --head --output /dev/null --write-out '%{http_code}' \
  "$package/$arch-$sha256/$file")
case $status in
  200) echo "$file $sha256 is already published" ;;
  404)
    printf '%s\t%s\n' "$raw" "$package/$arch-$sha256/$file" > "$uploads"
    TILO_PACKAGE_UPLOAD_WORKERS=1 bash tools/upload_immutable_packages.sh "$uploads" ;;
  *) die "asking for $package/$arch-$sha256/$file answered HTTP $status" ;;
esac
printf '%s\t%s\n' "$descriptor" "$package/$arch-$revision/descriptor.json" > "$uploads"
bash tools/upload_immutable_packages.sh "$uploads"
printf 'published the Windows compatibility extension for %s %s: %s (%s bytes, sha256 %s)\n' \
  "$arch" "$revision" "$package/$arch-$sha256/$file" "$size" "$sha256"
