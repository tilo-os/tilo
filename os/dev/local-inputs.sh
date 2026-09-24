#!/bin/bash
# Resolve the inputs of a local OS build, so no one assembles them by hand.
# Sourced by ./os/build.sh --local and os/dev/build-dev-image.sh before they
# build the production image here; it exports what os/dev/build-image.sh reads
# and says what it chose and why. Anything already set in the environment wins.
#
#   RPM snapshot   the published one (os/rpms-snapshot.env) when it carries this
#                  checkout's package set; otherwise os/dev/build-local-rpms.sh
#                  builds one, reused while the package set is unchanged
#                  (TILO_RPMS_TRUST=unsigned).
#   kernel         the pinned signed one (os/kernel-snapshot.env) when the
#                  registry still has it; otherwise the newest CI-built kernel
#                  for this architecture whose signed provenance names the
#                  current base and RPM snapshot; otherwise a local build of
#                  os/kernel/Containerfile, reused while its inputs are unchanged.
#                  TILO_KERNEL_TRUST=unsigned only when the chosen kernel cannot
#                  pass the signed check (a local kernel, or a local RPM snapshot
#                  its provenance cannot name).
#   owner key      the RFC 8032 test-vector key the CI build-validation lane uses
#                  (.forgejo/workflows/build-image.yml), never a production key.
#   series         TILO_ALLOW_EMPTY_SERIES=1, as in that lane.
#   browser        TILO_WITH_BROWSER=0 (the WPE engine is ~45 minutes cold).
#   Rust builder   TILO_CARGO_FRESHNESS=content: the builder keeps its target
#                  cache and recompiles only what changed (os/Containerfile).
#
# The kernel choice is cached in $TILO_OUTPUT/local-inputs/, keyed by the base,
# the RPM snapshot and the pin, so a rebuild does not ask the registry again.

_li_arch="$(uname -m)"
case "$_li_arch" in
  x86_64) _li_pin_var=KERNEL_RPMS_X86_64_REF ;;
  aarch64) _li_pin_var=KERNEL_RPMS_AARCH64_REF ;;
  *) die "local builds support x86_64 and aarch64, not $_li_arch" ;;
esac
# shellcheck disable=SC1091
. "$TILO_OS_DIR/base-snapshot.env"
_li_published_rpms="$(sed -n 's/^RPMS_REF=//p' "$TILO_OS_DIR/rpms-snapshot.env")"
_li_pin="$(sed -n "s/^$_li_pin_var=//p" "$TILO_OS_DIR/kernel-snapshot.env")"
_li_cache="$TILO_OUTPUT/local-inputs"
mkdir -p "$_li_cache"

: "${TILO_WITH_BROWSER:=0}"
: "${TILO_ALLOW_EMPTY_SERIES:=1}"
if [ -z "${TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY:-}" ]; then
  # RFC 8032 test vector 1: public, non-production, for build validation only.
  TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
  msg "local inputs: owner key = the RFC 8032 non-production test key (as CI build validation)"
fi
# Reuse the builder's target cache for unchanged sources (os/scripts/cargo-freshness.sh)
# instead of cleaning it; CI keeps the default `clean`.
case " ${TILO_BUILD_ARGS:-} " in
  *TILO_CARGO_FRESHNESS=*) ;;
  *) TILO_BUILD_ARGS="${TILO_BUILD_ARGS:-} --build-arg TILO_CARGO_FRESHNESS=${TILO_CARGO_FRESHNESS:-content}" ;;
esac
export TILO_WITH_BROWSER TILO_ALLOW_EMPTY_SERIES TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY TILO_BUILD_ARGS

# --- RPM snapshot --------------------------------------------------------------
if [ -n "${RPMS_REF:-}" ]; then
  msg "local inputs: RPM snapshot = $RPMS_REF (from the environment)"
elif bash "$TILO_OS_DIR/dev/verify-rpm-snapshot-package-set.sh" podman "$_li_published_rpms" >/dev/null 2>&1; then
  RPMS_REF="$_li_published_rpms"
  msg "local inputs: RPM snapshot = the published $RPMS_REF (it carries this checkout's package set)"
else
  msg "local inputs: the published RPM snapshot lacks this checkout's package set; building a local one"
  RPMS_REF="$("$TILO_OS_DIR/dev/build-local-rpms.sh")" || die "the local RPM snapshot build failed (see above)"
  TILO_RPMS_TRUST=unsigned
  msg "local inputs: RPM snapshot = $RPMS_REF (local, TILO_RPMS_TRUST=unsigned)"
fi
: "${TILO_RPMS_TRUST:=signed}"
export RPMS_REF TILO_RPMS_TRUST

# --- kernel --------------------------------------------------------------------
# Does the kernel's signed provenance name this base and RPM snapshot?
_li_kernel_binds() {
  command -v cosign >/dev/null || return 1
  cosign verify --key "$TILO_REPO/keys/cosign.pub" "$1" >/dev/null 2>&1 || return 1
  cosign verify-attestation --key "$TILO_REPO/keys/cosign.pub" --type slsaprovenance1 "$1" 2>/dev/null |
    jq -e --arg base "$BASE_REF" --arg rpms "$_li_published_rpms" '
      [(if type == "array" then .[] else . end) | .payload | @base64d | fromjson | .predicate]
      | any(any(.buildDefinition.resolvedDependencies[]?; .uri == $base)
            and any(.buildDefinition.resolvedDependencies[]?; .uri == $rpms))' >/dev/null 2>&1
}
_li_exists() {
  podman image exists "$1" 2>/dev/null || skopeo inspect --raw "docker://$1" >/dev/null 2>&1
}
_li_newest_ci_kernel() {
  local repo=code.sandbox.example/qos/tilo-kernel tag
  skopeo list-tags "docker://$repo" 2>/dev/null |
    jq -r --arg arch "$_li_arch" '.Tags[] | select(test("^main-\($arch)-[0-9a-f]+$") or test("^native-[0-9a-f]{40}-\($arch)$"))' |
    while read -r tag; do
      skopeo inspect "docker://$repo:$tag" 2>/dev/null | jq -r '"\(.Created) \(.Digest)"'
    done | sort -r | awk '!seen[$2]++ {print $2}' | head -n 6 |
    while read -r digest; do
      if _li_kernel_binds "$repo@$digest"; then echo "$repo@$digest"; return; fi
    done
}

_li_kernel_note=""
if [ -n "${KERNEL_RPMS_REF:-}" ]; then
  _li_kernel_note="from the environment"
else
  _li_key="$(printf '%s\n' "$_li_arch" "$BASE_REF" "$_li_published_rpms" "$_li_pin" | sha256sum | cut -c1-16)"
  _li_cached="$_li_cache/kernel-$_li_key"
  if [ -s "$_li_cached" ] && _li_ref="$(head -1 "$_li_cached")" && podman image exists "$_li_ref" 2>/dev/null; then
    KERNEL_RPMS_REF="$_li_ref"
    _li_kernel_note="$(sed -n 2p "$_li_cached") (cached choice)"
  elif [ -n "$_li_pin" ] && _li_exists "$_li_pin"; then
    KERNEL_RPMS_REF="$_li_pin"
    _li_kernel_note="the pinned signed kernel (os/kernel-snapshot.env)"
  else
    [ -z "$_li_pin" ] || msg "local inputs: the pinned kernel $_li_pin is gone from the registry; looking for the newest CI-built one"
    KERNEL_RPMS_REF="$(_li_newest_ci_kernel || true)"
    if [ -n "$KERNEL_RPMS_REF" ]; then
      _li_kernel_note="the newest CI-built $_li_arch kernel whose signed provenance names this base and RPM snapshot"
    else
      _li_kernel_hash="$( { printf '%s\n' "$BASE_REF" "$RPMS_REF"; git -C "$TILO_REPO" ls-files -s -- os/kernel os/external-assets/github-assets.tsv; } | sha256sum | cut -c1-16)"
      KERNEL_RPMS_REF="localhost/tilo-kernel:$_li_arch-$_li_kernel_hash"
      if podman image exists "$KERNEL_RPMS_REF"; then
        _li_kernel_note="a local build of os/kernel/Containerfile, reused (its inputs are unchanged)"
      else
        msg "local inputs: no usable CI kernel; building os/kernel/Containerfile locally (long, cold)"
        "$TILO_OS_DIR/dev/cache-github-assets.sh" v4l2loopback-0.15.3-a7e4a4d.tar.gz >&2
        podman build -f "$TILO_OS_DIR/kernel/Containerfile" \
          --build-context "github-assets=$TILO_OS_DIR/external-assets/cache" \
          --build-arg "BASE_REF=$BASE_REF" --build-arg "RPMS_REF=$RPMS_REF" \
          --build-arg "SOURCE_DATE_EPOCH=$(git -C "$TILO_REPO" log -1 --format=%ct)" \
          --tag "$KERNEL_RPMS_REF" "$TILO_REPO" >&2 || die "the local kernel build failed (see above)"
        _li_kernel_note="a local build of os/kernel/Containerfile"
      fi
      TILO_KERNEL_TRUST=unsigned
    fi
    printf '%s\n%s\n' "$KERNEL_RPMS_REF" "$_li_kernel_note" > "$_li_cached"
  fi
fi
if [ -z "${TILO_KERNEL_TRUST:-}" ]; then
  case "$KERNEL_RPMS_REF" in
    code.sandbox.example/qos/tilo-kernel@sha256:*)
      if [ "$TILO_RPMS_TRUST" = signed ] && _li_kernel_binds "$KERNEL_RPMS_REF"; then
        TILO_KERNEL_TRUST=signed
      else
        TILO_KERNEL_TRUST=unsigned
        _li_kernel_note="$_li_kernel_note; unsigned, because its provenance cannot name the RPM snapshot $RPMS_REF"
      fi ;;
    *) TILO_KERNEL_TRUST=unsigned ;;
  esac
fi
msg "local inputs: kernel = $KERNEL_RPMS_REF ($_li_kernel_note, TILO_KERNEL_TRUST=$TILO_KERNEL_TRUST)"
msg "local inputs: browser=$TILO_WITH_BROWSER, TILO_ALLOW_EMPTY_SERIES=$TILO_ALLOW_EMPTY_SERIES"
export KERNEL_RPMS_REF TILO_KERNEL_TRUST
