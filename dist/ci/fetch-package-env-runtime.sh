#!/usr/bin/env bash
# Stage the published env-runtime environment image for the tilo-session-apps
# RPM (dist/rpm/build/env-runtime), so a first environment opens offline. It is
# resolved and Cosign-verified on the host; the Fedora package container's
# source copy carries it to dist/rpm/build-rpm.sh. Without a published image
# the RPM ships none.
#
# usage: dist/ci/fetch-package-env-runtime.sh   (from the repository root;
#        needs skopeo, installs it with apt-get or dnf through sudo if missing)
set -euo pipefail

if ! command -v skopeo >/dev/null; then
  if command -v apt-get >/dev/null; then
    sudo -n apt-get update
    sudo -n apt-get install -y --no-install-recommends skopeo
  else
    sudo -n dnf install -y skopeo
  fi
fi
export RUNNER_TEMP=${RUNNER_TEMP:-$(mktemp -d)}
./os/dev/install-cached-cosign.sh
export PATH="$RUNNER_TEMP/tilo-cosign-tools:$PATH"
rm -rf dist/rpm/build/env-runtime
status=0
./os/dev/fetch-env-runtime.sh dist/rpm/build/env-runtime || status=$?
case $status in
  0) ;;
  1) echo "::warning::no env-runtime is published for os/Containerfile.env-runtime yet; the RPM ships none" ;;
  *) exit "$status" ;;
esac
