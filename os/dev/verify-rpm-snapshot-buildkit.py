#!/usr/bin/env python3
"""Verify a frozen RPM snapshot's package manifest from its metadata layer alone.

The snapshot's last layer (its repo metadata) carries
/repo/tilo-direct-package-specs.txt. Reading that one digest-checked blob
avoids pulling and unpacking the multi-GB package layers just to compare a few
kilobytes.
"""

import argparse
import difflib
import hashlib
import gzip
import io
import json
import os
import platform
from pathlib import Path
import re
import subprocess
import sys
import tarfile
from urllib.error import HTTPError
from urllib.parse import urlencode, urlparse
from urllib.request import Request, urlopen

MANIFEST_PATH = "repo/tilo-direct-package-specs.txt"
MAX_METADATA_LAYER = 64 * 1024 * 1024
MAX_METADATA_CONTENT = 256 * 1024 * 1024
MANIFEST_TYPES = {"application/vnd.oci.image.manifest.v1+json",
                  "application/vnd.docker.distribution.manifest.v2+json"}
INDEX_TYPES = {"application/vnd.oci.image.index.v1+json",
               "application/vnd.docker.distribution.manifest.list.v2+json"}
LAYER_TYPES = {"application/vnd.oci.image.layer.v1.tar+gzip",
               "application/vnd.docker.image.rootfs.diff.tar.gzip"}
CHALLENGE = re.compile(r'Bearer realm="(?P<realm>[^"]+)"(?:,service="(?P<service>[^"]*)")?'
                       r'(?:,scope="(?P<scope>[^"]*)")?')


def raw_manifest(ref):
    """The manifest bytes a digest-pinned ref names, checked against the digest."""
    raw = subprocess.check_output(["docker", "buildx", "imagetools", "inspect", "--raw", ref])
    digest = ref.rsplit("@", 1)[1]
    if "sha256:" + hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError(f"registry returned a manifest that is not {digest}")
    return json.loads(raw)


def platform_manifest(ref, arch):
    document = raw_manifest(ref)
    kind = document.get("mediaType") if isinstance(document, dict) else None
    if kind in MANIFEST_TYPES:
        config = document.get("config", {}).get("digest", "")
        metadata = json.loads(subprocess.check_output(
            ["docker", "buildx", "imagetools", "inspect", ref, "--format", "{{json .Image}}"], text=True))
        if not isinstance(metadata, dict) or metadata.get("architecture") != arch or metadata.get("os") != "linux":
            raise ValueError(f"frozen RPM snapshot is not a native linux/{arch} image ({config})")
        return document
    if kind not in INDEX_TYPES:
        raise ValueError("frozen RPM snapshot is neither an image index nor an image manifest")
    variants = {"", "v8"} if arch == "arm64" else {""}
    matches = [m for m in document.get("manifests", [])
               if isinstance(m, dict) and m.get("platform", {}).get("os") == "linux"
               and m["platform"].get("architecture") == arch
               and m["platform"].get("variant", "") in variants]
    if len(matches) != 1 or not re.fullmatch(r"sha256:[0-9a-f]{64}", str(matches[0].get("digest"))):
        raise ValueError(f"frozen RPM snapshot has no single linux/{arch} manifest")
    manifest = raw_manifest(ref.rsplit("@", 1)[0] + "@" + matches[0]["digest"])
    if manifest.get("mediaType") not in MANIFEST_TYPES:
        raise ValueError("frozen RPM snapshot platform entry is not an image manifest")
    return manifest


def docker_basic(host):
    config = Path(os.environ.get("DOCKER_CONFIG", Path.home() / ".docker")) / "config.json"
    try:
        auth = json.loads(config.read_text()).get("auths", {}).get(host, {}).get("auth")
    except (OSError, ValueError):
        return None
    return auth if isinstance(auth, str) and auth else None


def fetch_blob(host, repository, digest, size):
    url = f"https://{host}/v2/{repository}/blobs/{digest}"

    def get(authorization=None):
        request = Request(url)
        if authorization:
            # Not carried across a redirect to the blob store.
            request.add_unredirected_header("Authorization", authorization)
        with urlopen(request, timeout=120) as response:
            return response.read(MAX_METADATA_LAYER + 1)

    try:
        blob = get()
    except HTTPError as error:
        challenge = CHALLENGE.match(error.headers.get("WWW-Authenticate", "") if error.headers else "")
        error.close()
        if error.code != 401 or not challenge:
            raise
        query = urlencode({k: v for k, v in (("service", challenge["service"]),
                                              ("scope", f"repository:{repository}:pull")) if v})
        token_request = Request(challenge["realm"] + "?" + query)
        basic = docker_basic(host)
        # Credentials go only to the registry's own token service.
        if basic and urlparse(challenge["realm"]).hostname == host:
            token_request.add_unredirected_header("Authorization", "Basic " + basic)
        with urlopen(token_request, timeout=30) as response:
            document = json.loads(response.read(1024 * 1024))
        token = document.get("token") or document.get("access_token")
        if not isinstance(token, str) or not token:
            raise ValueError("registry returned no pull token")
        blob = get("Bearer " + token)
    if len(blob) != size or "sha256:" + hashlib.sha256(blob).hexdigest() != digest:
        raise ValueError(f"metadata layer {digest} does not match its manifest entry")
    return blob


def verify(ref):
    arch = {"x86_64": "amd64", "aarch64": "arm64"}.get(platform.machine())
    if arch is None:
        raise ValueError("RPM snapshot verification requires a supported native build host")
    match = re.fullmatch(r"(?P<host>[^/\s@]+)/(?P<repo>[^\s@]+)@sha256:[0-9a-f]{64}", ref)
    if not match:
        raise ValueError("RPM snapshot must be pinned by its sha256 digest")
    manifest = platform_manifest(ref, arch)
    layers = manifest.get("layers") or []
    last = layers[-1] if layers else {}
    if (last.get("mediaType") not in LAYER_TYPES or not isinstance(last.get("size"), int)
            or not 0 < last["size"] <= MAX_METADATA_LAYER
            or not re.fullmatch(r"sha256:[0-9a-f]{64}", str(last.get("digest")))):
        raise ValueError("frozen RPM snapshot has no bounded metadata layer last")
    blob = fetch_blob(match["host"], match["repo"], last["digest"], last["size"])
    # A compressed-byte limit alone does not bound tar parsing or extraction.
    with gzip.GzipFile(fileobj=io.BytesIO(blob)) as compressed:
        content = compressed.read(MAX_METADATA_CONTENT + 1)
    if len(content) > MAX_METADATA_CONTENT:
        raise ValueError("expanded metadata layer exceeds its size bound")
    with tarfile.open(fileobj=io.BytesIO(content), mode="r:") as layer:
        members = [m for m in layer.getmembers()
                   if Path(m.name).as_posix() == MANIFEST_PATH]
        if len(members) != 1 or not members[0].isreg():
            raise ValueError("frozen RPM snapshot has no regular package manifest")
        actual = layer.extractfile(members[0]).read().decode("utf-8")
    repo = Path(__file__).resolve().parents[2]
    expected = subprocess.check_output(
        [sys.executable, str(repo / "os/dev/derive-rpm-package-set.py")], text=True,
    )
    if actual != expected:
        sys.stderr.writelines(difflib.unified_diff(
            actual.splitlines(keepends=True), expected.splitlines(keepends=True),
            fromfile="snapshot", tofile="current consumers",
        ))
        raise ValueError("frozen RPM snapshot does not match the current Containerfiles")
    print("Frozen RPM snapshot package manifest matches every consumer.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ref")
    args = parser.parse_args()
    try:
        verify(args.ref)
    except (ValueError, OSError, subprocess.CalledProcessError, tarfile.TarError) as error:
        print(f"RPM snapshot verification failed: {error}", file=sys.stderr)
        sys.exit(1)
