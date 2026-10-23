#!/usr/bin/env bash
#   tools/ci/cloud-conformance.sh servers SUITE   # real servers in containers (root: podman)
#   tools/ci/cloud-conformance.sh live SUITE      # live accounts, from TILO_LIVE_* secrets
#
# The suite skips a layer whose variables are absent, which is right on a
# developer's machine and wrong in t's cloud-servers and cloud-live checks:
# there an absent server, a missing secret or a filter that matches fewer tests
# than the binary lists fails.
set -euo pipefail

usage() {
  echo "usage: $0 servers|live SUITE" >&2
  exit 2
}

[ "$#" -eq 2 ] || usage
layer=$1
suite=$(realpath "$2")
root=$(cd "$(dirname "$0")/../.." && pwd)
crate=$root/crates/system/cloud-source-dav

# Run every test of the suite whose name starts with FILTER, and fail unless
# all of them ran and passed: a filter that matches nothing still exits 0.
run_layer() {
  local filter=$1 expected log actual
  shift
  expected=$("$suite" "$filter" --list | grep -c ': test$' || true)
  if [ "$expected" -eq 0 ]; then
    echo "cloud-conformance: $suite lists no $filter tests" >&2
    exit 1
  fi
  log=$(mktemp)
  (cd "$crate" && "$suite" "$filter" --nocapture "$@") 2>&1 | tee "$log"
  actual=$(sed -n 's/^test result: ok\. \([0-9][0-9]*\) passed.*/\1/p' "$log" | tail -n 1)
  rm -f "$log"
  if [ "${actual:-}" != "$expected" ]; then
    echo "cloud-conformance: expected $expected passing $filter tests, got ${actual:-none}" >&2
    exit 1
  fi
}

case "$layer" in
  servers)
    if [ "$(id -u)" != 0 ]; then
      echo "cloud-conformance: the servers layer runs rootful podman; expected euid 0, got $(id -u)" >&2
      exit 1
    fi
    # A micro-VM runs no systemd: podman manages cgroups itself and logs
    # events to a file.
    mkdir -p /etc/containers/containers.conf.d
    printf '[engine]\ncgroup_manager = "cgroupfs"\nevents_logger = "file"\n' \
      > /etc/containers/containers.conf.d/50-micro-vm.conf
    command -v podman >/dev/null \
      || dnf install -y --setopt=install_weak_deps=False podman >/dev/null
    servers=$crate/tests/servers.sh
    trap 'bash "$servers" stop' EXIT
    eval "$(bash "$servers" start)"
    # Without a server's variables the suite fails instead of skipping.
    export TILO_REQUIRE_CLOUD_SERVERS=1
    run_layer container_
    ;;
  live)
    missing=
    for name in TILO_LIVE_WEBDAV_URL TILO_LIVE_WEBDAV_USER TILO_LIVE_WEBDAV_PASSWORD \
      TILO_LIVE_NEXTCLOUD_URL TILO_LIVE_NEXTCLOUD_USER TILO_LIVE_NEXTCLOUD_PASSWORD; do
      [ -n "${!name:-}" ] || missing="$missing $name"
    done
    if [ -n "$missing" ]; then
      echo "cloud-conformance: the live layer needs these secrets from t's server config:$missing" >&2
      exit 1
    fi
    # Each test works in a folder of its own on the shared accounts.
    run_layer live_ --test-threads=1
    ;;
  *)
    usage
    ;;
esac
