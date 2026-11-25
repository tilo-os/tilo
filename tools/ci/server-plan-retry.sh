#!/usr/bin/env bash
# A retry of the server install proof (os/dev/test-server-plan-install-cycle.sh) from a base,
# rebuilding only the components this tree changed.
#
#   server-plan-retry.sh layers OUT             what to rebuild, and its layers (server-plan-retry-layers)
#   server-plan-retry.sh fetch OUT              the base, unpacked (server-plan-base-fetch)
#   server-plan-retry.sh proof LAYERS BASE OUT  the proof run on BASE with LAYERS
#                                               (server-plan-install-retry)
#
# The base is what publish-server-plan-base built from one commit of main and pushed as one
# image: the installer medium's bundle with a development server image as its target, the test
# Imager that names that bundle, and the owner's dev disk. tools/ci/server-plan-base.env names
# its commit and the image by digest, so the base moves only when someone dispatches it again
# and commits that file, never with main; the retry reads it from the tree under test.
#
# What differs from that commit (tools/ci/server-plan-retry.py select) is built as tilo-vm
# layers, the ones `tilo-vm swap` installs (os/components.toml), with the compiler cache and
# without link-time optimisation. The installed server takes them through the live installer,
# which copies them into its tilo-vm state before its first boot; tilo-vm-incoming.service merges
# them there before tilo-server starts. The owner's dev disk takes them the same way before its
# session starts. A change the base would have to be rebuilt for (a program the sealed medium
# runs, early boot, the OS images' own files) stops the retry before anything boots, and says
# which and why.
#
# OUT/timings.tsv records every step's seconds; the log ends with them as a table.
set -euo pipefail
forge=https://code.tilo.so/tilo/tilo.git
die() { printf 'server plan retry: %s\n' "$*" >&2; exit 1; }
note() { printf 'server plan retry: %s\n' "$*" >&2; }
repo=$(cd "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
mode=${1:?usage: server-plan-retry.sh layers OUT | fetch OUT | proof LAYERS BASE OUT}
case $mode in
  layers | fetch) out=${2:?} ;;
  proof) built=${2:?} fetched=${3:?} out=${4:?} ;;
  *) die "unknown mode $mode" ;;
esac

rm -rf "$out"
mkdir -p "$out"
timings=$out/timings.tsv
: > "$timings"
step_name="" step_started=$SECONDS
step() {
  [[ -z $step_name ]] || printf '%s\t%s\n' "$step_name" "$((SECONDS - step_started))" >> "$timings"
  step_name=$1 step_started=$SECONDS
  [[ -z $step_name ]] || note "step: $step_name"
}
table() {
  step ""
  printf '\n%-28s %8s\n' step seconds
  awk -F'\t' '{ printf "%-28s %8s\n", $1, $2; total += $2 } END { printf "%-28s %8s\n", "total", total }' "$timings"
}
trap table EXIT

base=$repo/tools/ci/server-plan-base.env
read_base() {
  [[ -s $base ]] || die "no base: dispatch publish-server-plan-base on main (t dispatch --repo tilo --ref main@green publish-server-plan-base --publish) and commit the COMMIT= and IMAGE= lines its log ends with as ${base#"$repo"/}"
  base_commit=$(sed -n 's/^COMMIT=//p' "$base") base_image=$(sed -n 's/^IMAGE=//p' "$base")
  [[ $base_commit =~ ^[0-9a-f]{40}$ ]] || die "${base#"$repo"/} names no commit"
  [[ $base_image =~ ^registry\.tilo\.so/[a-z0-9/_-]+@sha256:[0-9a-f]{64}$ ]] \
    || die "${base#"$repo"/} names no image by digest"
  printf 'base: %s (%s)\n' "$base_commit" "$base_image"
}

if [[ $mode == layers ]]; then
  step base
  read_base
  git -C "$repo" cat-file -e "$base_commit^{commit}" 2>/dev/null \
    || git -C "$repo" fetch -q --depth=1 --no-tags "$forge" "$base_commit" \
    || die "the base commit $base_commit is not on the forge"
  step select
  python3 "$repo/tools/ci/server-plan-retry.py" select --base "$base_commit" > "$out/selection.json"
  components=$(jq -r '.components | join(" ")' "$out/selection.json")
  printf 'components: %s\n' "${components:-none}"
  printf 'into the installed server: %s\n' "$(jq -r '.server | join(" ")' "$out/selection.json")"
  printf 'into the owner device: %s\n' "$(jq -r '.owner | join(" ")' "$out/selection.json")"
  printf 'from the tree as they are: %s\n' "$(jq -r '.direct | join(" ")' "$out/selection.json")"
  if jq -e '.refused | length > 0' "$out/selection.json" >/dev/null; then
    jq -r '.refused[] | "  \(.path // .component)\(if .programs then " (" + (.programs | join(", ")) + ")" else "" end): \(.reason)"' \
      "$out/selection.json" >&2
    die "this change needs a new base, which this retry would not test: land it, then dispatch publish-server-plan-base from the newest green commit of main (--ref main@green)"
  fi
  step build
  layers=$out/layers
  mkdir -p "$layers"
  for component in $components; do
    # tilo-vm's own packer: the same build and layout `tilo-vm swap --builder remote` makes.
    bash "$repo/tools/ci/server-plan-layer.sh" "$component" "$layers"
  done
  exit 0
fi

if [[ $mode == fetch ]]; then
  # Outside the proof's VM, whose user-mode network moves a few MB/s; kept by t under the base
  # file's key, so retries on one base fetch it once.
  step base
  read_base
  step fetch
  python3 "$repo/tools/ci/publish/server-plan-base.py" fetch "$base_image" "$out/base" > "$out/base-config.json"
  [[ $(jq -r .commit "$out/base-config.json") == "$base_commit" ]] \
    || die "the base image was built from $(jq -r .commit "$out/base-config.json"), not $base_commit"
  du -sh --apparent-size "$out/base"
  exit 0
fi

step base
read_base
[[ $(jq -r .base "$built/selection.json") == "$base_commit" ]] \
  || die "the layers were selected against $(jq -r .base "$built/selection.json"), not the base $base_commit"
[[ $(jq -r .commit "$fetched/base-config.json") == "$base_commit" ]] \
  || die "the fetched base is of $(jq -r .commit "$fetched/base-config.json"), not $base_commit"
base_dir=$fetched/base
printf 'components swapped: %s\n' "$(jq -r '.components | join(" ")' "$built/selection.json")"

step proof
layers=$built/layers
selection=$built/selection.json
server_layers=$out/server-layers owner_layers=$out/owner-layers
mkdir -p "$server_layers" "$owner_layers"
for component in $(jq -r '.server[]' "$selection"); do cp "$layers/$component".* "$server_layers/"; done
for component in $(jq -r '.owner[]' "$selection"); do cp "$layers/$component".* "$owner_layers/"; done
args=(--bundle-dir "$base_dir" --imager "$base_dir/imager" --owner-disk "$base_dir/owner-disk.qcow2"
      --release-tag imager-v0.0.0 --out "$out/proof")
[[ -z $(ls -A "$server_layers") ]] || args+=(--development-layers "$server_layers")
[[ -z $(ls -A "$owner_layers") ]] || args+=(--owner-layers "$owner_layers")
proof_steps() {
  # The proof's own stages, as the seconds since the one before (its evidence/steps.jsonl).
  local steps=$out/proof/evidence/steps.jsonl
  [[ -s $steps ]] || return 0
  printf '\nproof stages (seconds since the stage before, from the proof start):\n'
  jq -rs --argjson start "$proof_started" \
    'reduce .[] as $s ({last: $start, rows: []}; {last: $s.at, rows: (.rows + ["\($s.step)\t\($s.at - .last)"])}) | .rows[]' \
    "$steps" | awk -F'\t' '{ printf "  %-26s %8s\n", $1, $2 }'
}
proof_started=$(date +%s)
trap 'proof_steps; table' EXIT
bash "$repo/os/dev/test-server-plan-install-cycle.sh" "${args[@]}"
