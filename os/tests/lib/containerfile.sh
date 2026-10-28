#!/usr/bin/env bash
# Helpers for inspecting Dockerfile/Containerfile logical instructions.
# Multiline COPY is intentional in the production image: each grouped COPY is
# one cacheable layer, so line-oriented grep/awk is not a faithful parser.

containerfile_instructions() {
  awk '
    function emit() {
      if (logical != "") print logical
      logical = ""
    }
    {
      line = $0
      sub(/^[[:space:]]+/, "", line)
      continued = sub(/[[:space:]]*\\[[:space:]]*$/, "", line)
      if (logical == "") logical = line
      else logical = logical " " line
      if (!continued) emit()
    }
    END { emit() }
  ' "$1"
}

# The stages whose instructions make the image's own layers, space-separated. The
# OS image is built as layers (os/image/layers.toml), one piece each, so its rootfs
# is the stages of every piece (tools/ci/os/pieces.py stages); any other
# Containerfile's is its rootfs stage and the base that stage builds on.
containerfile_image_stages() {
  local file="$1" os_dir
  os_dir="$(cd "$(dirname "$file")" && pwd)"
  if [ "$(basename "$file")" = Containerfile ] && [ -f "$os_dir/image/layers.toml" ]; then
    python3 "$os_dir/../tools/ci/os/pieces.py" stages | tr '\n' ' '
  else
    printf 'rootfs-base rootfs'
  fi
}

# Emit the effective file-copy contract as:
#   source-stage<TAB>source<TAB>destination
# The `context` source stage denotes the build context. Grouped COPY destinations
# must be directories; Docker installs each source under its basename there.
# The stage `rootfs` stands for every stage of the image's layers
# (containerfile_image_stages).
containerfile_copy_contract() {
  file="$1" wanted_stage="${2:-}" stages=""
  [ "$wanted_stage" != rootfs ] || stages=" $(containerfile_image_stages "$file") "
  containerfile_instructions "$file" | awk -v wanted="$wanted_stage" -v stages="$stages" '
    $1 == "FROM" { current = $NF; next }
    $1 == "COPY" && (wanted == "" || current == wanted || index(stages, " " current " ")) {
      stage = "context"; first = 2
      while (first <= NF && substr($first, 1, 2) == "--") {
        if ($first ~ /^--from=/) { stage = $first; sub(/^--from=/, "", stage) }
        first++
      }
      destination = $NF
      sources = NF - first
      for (i = first; i < NF; i++) {
        effective = destination
        if (sources > 1) {
          base = $i; sub(/^.*\//, "", base)
          effective = destination base
        }
        printf "%s\t%s\t%s\n", stage, $i, effective
      }
    }
  ' | containerfile_expand_component_layers "$file"
}

# `COPY --from=component-files /image/<layer>/ /` installs one layer of
# os/components.toml. Expand it into the manifest's own entries, as
#   components.toml<TAB><component>:<from | @link:target><TAB><image path>
# so contracts and has-copy checks see each file rather than an opaque tree.
# The manifest sits next to the Containerfile. A manifest that cannot be read
# yields a row no check can match, so dependent checks fail rather than pass.
containerfile_expand_component_layers() {
  local file="$1" os_dir stage source destination layer
  os_dir="$(cd "$(dirname "$file")" && pwd)"
  while IFS=$'\t' read -r stage source destination; do
    if [ "$stage" = component-files ] && [ "$destination" = / ] \
       && [[ "$source" =~ ^/image/([a-z0-9-]+)/$ ]]; then
      layer="${BASH_REMATCH[1]}"
      python3 "$os_dir/scripts/install-components.py" \
          --manifest "$os_dir/components.toml" \
          --dry-run --all --layer "$layer" --format contract \
        || printf 'component-files\tunreadable-manifest:%s\t%s\n' "$layer" "$destination"
    else
      printf '%s\t%s\t%s\n' "$stage" "$source" "$destination"
    fi
  done
}

# True when the image installs build output OUTPUT (a name under the builder's
# /out, or a manifest `from`) at DESTINATION, whether through a direct builder
# COPY or through os/components.toml.
containerfile_installs_output() {
  local file="$1" output="$2" destination="$3"
  containerfile_expanded_rootfs_copy_contract "$file" |
    awk -F '\t' -v o="$output" -v d="$destination" '
      $3 != d { next }
      $1 == "builder" && $2 == "/out/" o { found=1 }
      $1 == "components.toml" && substr($2, index($2, ":") + 1) == o { found=1 }
      END { exit !found }'
}

# Expand the deliberately narrow whole-tree scratch overlays used by rootfs.
# No generic recursion is intended: accepting an arbitrary `COPY --from=X / /`
# here could make a new, unreviewed bulk overlay invisible to the contract test.
containerfile_expanded_rootfs_copy_contract() {
  file="$1"
  containerfile_copy_contract "$file" rootfs |
    while IFS=$'\t' read -r stage source destination; do
      case "$stage:$source:$destination" in
        desktop-binaries:/:/|remote-capture-files:/:/|rollback-files:/:/|recovery-files:/:/|extension-files:/:/)
          containerfile_copy_contract "$file" "$stage"
          ;;
        *) printf '%s\t%s\t%s\n' "$stage" "$source" "$destination" ;;
      esac
    done
}

containerfile_has_copy() {
  file="$1" source="$2" destination="$3" stage="${4:-context}"
  containerfile_copy_contract "$file" |
    awk -F '\t' -v s="$stage" -v p="$source" -v d="$destination" \
      '$1 == s && $2 == p && $3 == d { found=1 } END { exit !found }'
}
