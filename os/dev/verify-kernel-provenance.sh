#!/usr/bin/env bash
# Authenticate a native Tilo kernel repository before an OS build consumes it.
#
#   verify-kernel-provenance.sh <kernel-ref>
#
# Verifies the kernel's signature and its signed SLSA provenance, which must
# bind the qOS source commit, the native architecture and the kernel digest,
# plus exactly one build environment (os/dev/kernel-build-environment.py):
# the signed native kernel buildroot, or for kernels published before it, the
# signed RPM snapshot they were built from. That environment's own signature is
# verified too.
#
# The kernel is deliberately not compared with the OS build's RPM snapshot or
# base: the kernel builds on its own buildroot, so an OS package or base change
# never invalidates it. Publications and integration images check alike.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
key="$repo/keys/cosign.pub"
die() { printf '%s\n' "$*" >&2; exit 1; }

[ "$#" -eq 1 ] || die "usage: verify-kernel-provenance.sh <kernel-ref>"
ref=$1
case "$(uname -m)" in
  x86_64) expected_arch=amd64 ;;
  aarch64) expected_arch=arm64 ;;
  *) die "unsupported kernel architecture: $(uname -m)" ;;
esac
[[ "$ref" =~ ^code\.podesta\.ai/qos/tilo-kernel@sha256:[0-9a-f]{64}$ ]] \
  || die "no digest-pinned native Tilo kernel for $(uname -m)"

cosign verify --key "$key" "$ref" >/dev/null || die "Tilo kernel signature verification failed"
provenance="$(cosign verify-attestation --key "$key" --type slsaprovenance1 "$ref")" \
  || die "Tilo kernel provenance verification failed"

# The kernel's own identity (https://tilo.sh/buildtypes/kernel-rpm/v1: source,
# architecture, digest) and the one build environment it names.
environment="$(printf '%s' "$provenance" | python3 "$repo/os/dev/kernel-build-environment.py" \
  --digest "${ref##*@}" --architecture "$(uname -m)")" \
  || die "Tilo kernel provenance does not bind its source, architecture, digest, and one signed build environment"
cosign verify --key "$key" "$environment" >/dev/null \
  || die "the build environment $environment bound by the Tilo kernel is not signed by the release key"
case "$environment" in
  code.podesta.ai/qos/tilo-kernel-buildroot@*) ;;
  *) printf 'Tilo kernel was built from the signed RPM snapshot %s, before the kernel buildroot\n' "$environment" >&2 ;;
esac

actual_arch="$(skopeo inspect "docker://$ref" | jq -er .Architecture)"
[ "$actual_arch" = "$expected_arch" ] \
  || die "signed Tilo kernel architecture is $actual_arch, expected $expected_arch"
