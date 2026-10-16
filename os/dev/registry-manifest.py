#!/usr/bin/env python3
"""Read an image manifest from a registry anonymously, for hosts without skopeo.

  registry-manifest.py digest HOST/REPOSITORY:TAG|@DIGEST   print the manifest's digest
  registry-manifest.py raw HOST/REPOSITORY:TAG|@DIGEST      print the manifest itself

Exit 1 with "manifest unknown" when the registry has no such manifest, 3 when it could not be
asked, as skopeo's errors are told apart by the resolver scripts.
"""
from __future__ import annotations

import hashlib
import json
import re
import sys
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen

ACCEPT = ", ".join((
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.docker.distribution.manifest.v2+json",
))


def parse(ref: str) -> tuple[str, str, str]:
    ref = ref.removeprefix("docker://")
    host, _, rest = ref.partition("/")
    if "@" in rest:
        repository, reference = rest.split("@", 1)
    else:
        repository, _, reference = rest.rpartition(":")
    if not host or not repository or not reference:
        raise SystemExit(f"registry-manifest: not an image reference: {ref}")
    return host, repository, reference


def fetch(url: str, token: str | None = None) -> bytes:
    headers = {"Accept": ACCEPT}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    with urlopen(Request(url, headers=headers), timeout=60) as response:
        return response.read()


def manifest(ref: str) -> bytes:
    host, repository, reference = parse(ref)
    url = f"https://{host}/v2/{repository}/manifests/{reference}"
    try:
        return fetch(url)
    except HTTPError as error:
        if error.code != 401:
            raise
        challenge = error.headers.get("WWW-Authenticate", "")
    fields = dict(re.findall(r'(\w+)="([^"]*)"', challenge))
    if "realm" not in fields:
        raise SystemExit(f"registry-manifest: {host} asks for credentials this reader has not")
    query = {k: v for k, v in fields.items() if k in ("service", "scope")}
    query.setdefault("scope", f"repository:{repository}:pull")
    answer = json.loads(fetch(f"{fields['realm']}?{urlencode(query)}"))
    return fetch(url, answer.get("token") or answer.get("access_token"))


def main() -> int:
    if len(sys.argv) != 3 or sys.argv[1] not in ("digest", "raw"):
        print(__doc__, file=sys.stderr)
        return 2
    try:
        body = manifest(sys.argv[2])
    except HTTPError as error:
        if error.code == 404:
            print(f"registry-manifest: manifest unknown: {sys.argv[2]}", file=sys.stderr)
            return 1
        print(f"registry-manifest: {sys.argv[2]}: HTTP {error.code}", file=sys.stderr)
        return 3
    except (URLError, OSError, ValueError) as error:
        print(f"registry-manifest: {sys.argv[2]}: {error}", file=sys.stderr)
        return 3
    if sys.argv[1] == "digest":
        print("sha256:" + hashlib.sha256(body).hexdigest())
    else:
        sys.stdout.write(body.decode())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
