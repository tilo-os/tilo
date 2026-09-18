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

# Emit the effective file-copy contract as:
#   source-stage<TAB>source<TAB>destination
# The `context` source stage denotes the build context. Grouped COPY destinations
# must be directories; Docker installs each source under its basename there.
containerfile_copy_contract() {
  file="$1" wanted_stage="${2:-}"
  containerfile_instructions "$file" | awk -v wanted="$wanted_stage" '
    $1 == "FROM" { current = $NF; next }
    $1 == "COPY" && (wanted == "" || current == wanted) {
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
  '
}

# Expand the deliberately narrow whole-tree scratch overlays used by rootfs.
# No generic recursion is intended: accepting an arbitrary `COPY --from=X / /`
# here could make a new, unreviewed bulk overlay invisible to the contract test.
containerfile_expanded_rootfs_copy_contract() {
  file="$1"
  containerfile_copy_contract "$file" rootfs |
    while IFS=$'\t' read -r stage source destination; do
      case "$stage:$source:$destination" in
        desktop-binaries:/:/|remote-capture-files:/:/|rollback-files:/:/|recovery-files:/:/)
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
