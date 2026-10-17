#!/usr/bin/env bash
# Authenticate a native Tilo kernel repository before an OS build consumes it.
#
#   verify-kernel-provenance.sh <kernel-ref> [--architecture x86_64|aarch64]
#
# Verifies the kernel's signature and its signed SLSA provenance, which must
# bind the Tilo source commit, the native architecture and the kernel digest,
# plus exactly one signed native kernel buildroot
# (os/dev/kernel-build-environment.py), whose own signature is verified too.
#
# The kernel is deliberately not compared with the OS build's RPM snapshot or
# base: the kernel builds on its own buildroot, so an OS package or base change
# never invalidates it. Publications and integration images check alike.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
key="$repo/keys/cosign.pub"
die() { printf '%s\n' "$*" >&2; exit 1; }

[ "$#" -eq 1 ] || { [ "$#" -eq 3 ] && [ "$2" = --architecture ]; } \
  || die "usage: verify-kernel-provenance.sh <kernel-ref> [--architecture x86_64|aarch64]"
ref=$1
architecture=${3:-$(uname -m)}
case "$architecture" in
  x86_64) expected_arch=amd64 ;;
  aarch64) expected_arch=arm64 ;;
  *) die "unsupported kernel architecture: $architecture" ;;
esac
[[ "$ref" =~ ^registry\.tilo\.so/tilo/tilo-kernel@sha256:[0-9a-f]{64}$ ]] \
  || die "no digest-pinned native Tilo kernel for $architecture"

cosign verify --key "$key" "$ref" >/dev/null || die "Tilo kernel signature verification failed"
provenance="$(cosign verify-attestation --key "$key" --type slsaprovenance1 "$ref")" \
  || die "Tilo kernel provenance verification failed"

# The kernel's own identity (https://tilo.sh/buildtypes/kernel-rpm/v1: source,
# architecture, digest) and the one build environment it names.
environment="$(printf '%s' "$provenance" | python3 "$repo/os/dev/kernel-build-environment.py" \
  --digest "${ref##*@}" --architecture "$architecture")" \
  || die "Tilo kernel provenance does not bind its source, architecture, digest, and one signed kernel buildroot"
cosign verify --key "$key" "$environment" >/dev/null \
  || die "the kernel buildroot $environment bound by the Tilo kernel is not signed by the release key"

if command -v skopeo >/dev/null; then
  actual_arch="$(skopeo inspect "docker://$ref" | jq -er .Architecture)"
else
  # The image config's architecture, read anonymously from the registry.
  actual_arch="$(python3 - "$ref" <<'PY'
import json, re, sys, urllib.error, urllib.parse, urllib.request
name, digest = sys.argv[1].split("@")
host, repository = name.split("/", 1)
token = None
def get(path, accept):
    global token
    for attempt in range(2):
        request = urllib.request.Request(f"https://{host}/v2/{repository}/{path}", headers={"Accept": accept})
        if token:
            # Not forwarded when a blob redirects to storage that signs its own URLs.
            request.add_unredirected_header("Authorization", f"Bearer {token}")
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            challenge = error.headers.get("WWW-Authenticate", "")
            if error.code != 401 or attempt or not challenge.startswith("Bearer "):
                raise
            # An anonymous pull token, as the registry's challenge describes.
            fields = dict(re.findall(r'(\w+)="([^"]*)"', challenge))
            query = urllib.parse.urlencode({k: v for k, v in fields.items() if k in ("service", "scope")})
            with urllib.request.urlopen(f"{fields['realm']}?{query}", timeout=60) as response:
                answer = json.load(response)
            token = answer.get("token") or answer.get("access_token")
manifest = get(f"manifests/{digest}", "application/vnd.oci.image.manifest.v1+json, "
               "application/vnd.docker.distribution.manifest.v2+json")
config = get(f"blobs/{manifest['config']['digest']}", "*/*")
print(config["architecture"])
PY
)"
fi
[ "$actual_arch" = "$expected_arch" ] \
  || die "signed Tilo kernel architecture is $actual_arch, expected $expected_arch"
