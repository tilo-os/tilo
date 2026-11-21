#!/usr/bin/env python3
"""The base a retry of the server install proof starts from, as one OCI image.

    server-plan-base.py pack OUT COMMIT NAME=PATH...   write OUT/oci and OUT/digest
    server-plan-base.py unpack LAYOUT DEST             extract every part into DEST/NAME
    server-plan-base.py fetch IMAGE DEST               the same, streamed from the registry
                                                       (IMAGE is REPOSITORY@sha256:DIGEST)

`pack` makes each NAME=PATH (a file or a directory) one zstd-compressed tar layer, titled NAME,
of an image whose config records COMMIT, the commit every part was built from. oci-publish.sh
pushes OUT as it is. `unpack` reads a layout skopeo copied from the registry, checks every layer
against its digest while it extracts it, and refuses a layout whose parts are not named. `fetch`
does the same straight from the registry, with an anonymous pull token: no layout is written,
so each part is read once and written once.

Sparse files (the raw images of the installer bundle) stay sparse both ways: tar records the
holes, and nothing writes them as zeroes.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import shutil
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

LAYER = "application/vnd.oci.image.layer.v1.tar+zstd"
CONFIG = "application/vnd.tilo.server-plan-base.config.v1+json"
MANIFEST = "application/vnd.oci.image.manifest.v1+json"
TITLE = "org.opencontainers.image.title"


def die(message: str) -> None:
    raise SystemExit(f"server-plan-base: {message}")


def blob(layout: Path, data: bytes) -> dict:
    digest = hashlib.sha256(data).hexdigest()
    (layout / "blobs/sha256" / digest).write_bytes(data)
    return {"digest": f"sha256:{digest}", "size": len(data)}


def pack_part(layout: Path, name: str, path: Path) -> dict:
    if not path.exists():
        die(f"{name}: {path} does not exist")
    staging = layout / "blobs/sha256/.partial"
    with open(staging, "wb") as sink:
        tar = subprocess.Popen(
            ["tar", "--sparse", "--numeric-owner", "--owner=0", "--group=0", "-C", str(path.parent),
             "--transform", f"s,^{path.name},{name},", "-cf", "-", path.name],
            stdout=subprocess.PIPE)
        zstd = subprocess.Popen(["zstd", "-T0", "-3", "-q", "-c"], stdin=tar.stdout, stdout=subprocess.PIPE)
        tar.stdout.close()
        hasher, size = hashlib.sha256(), 0
        while chunk := zstd.stdout.read(1 << 22):
            hasher.update(chunk)
            sink.write(chunk)
            size += len(chunk)
        if zstd.wait() or tar.wait():
            die(f"{name}: packing {path} failed")
    digest = hasher.hexdigest()
    staging.rename(layout / "blobs/sha256" / digest)
    return {"mediaType": LAYER, "digest": f"sha256:{digest}", "size": size,
            "annotations": {TITLE: name}}


def pack(out: Path, commit: str, parts: list[str]) -> None:
    if len(commit) != 40 or any(c not in "0123456789abcdef" for c in commit):
        die(f"not a commit: {commit}")
    layout = out / "oci"
    shutil.rmtree(layout, ignore_errors=True)
    (layout / "blobs/sha256").mkdir(parents=True)
    layers, names = [], []
    for part in parts:
        name, _, path = part.partition("=")
        if not name or not path or "/" in name or name in names:
            die(f"a part is NAME=PATH with a unique plain name: {part}")
        names.append(name)
        layers.append(pack_part(layout, name, Path(path).resolve()))
    config = blob(layout, json.dumps({"commit": commit, "parts": names}, sort_keys=True).encode())
    manifest = json.dumps({"schemaVersion": 2, "mediaType": MANIFEST,
                           "config": {"mediaType": CONFIG, **config}, "layers": layers},
                          sort_keys=True).encode()
    described = blob(layout, manifest)
    (layout / "index.json").write_text(json.dumps(
        {"schemaVersion": 2, "manifests": [{"mediaType": MANIFEST, **described}]}))
    (layout / "oci-layout").write_text('{"imageLayoutVersion":"1.0.0"}')
    (out / "digest").write_text(described["digest"] + "\n")
    print(f"packed {', '.join(names)} of {commit} as {described['digest']}")


def extract(layer: dict, chunks, dest: Path) -> None:
    """One named part, its bytes from CHUNKS, extracted under DEST and checked against its digest."""
    name = layer.get("annotations", {}).get(TITLE, "")
    if layer.get("mediaType") != LAYER or not name or "/" in name:
        die(f"a layer of the base is not one of its named parts: {layer}")
    hasher = hashlib.sha256()
    zstd = subprocess.Popen(["zstd", "-d", "-q", "-c"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    tar = subprocess.Popen(["tar", "--sparse", "-C", str(dest), "-xf", "-"], stdin=zstd.stdout)
    zstd.stdout.close()
    for chunk in chunks:
        hasher.update(chunk)
        zstd.stdin.write(chunk)
    zstd.stdin.close()
    if zstd.wait() or tar.wait():
        die(f"{name}: extracting failed")
    if f"sha256:{hasher.hexdigest()}" != layer["digest"]:
        die(f"{name}: its bytes are not the layer the base names")


def file_chunks(path: Path):
    with open(path, "rb") as source:
        while chunk := source.read(1 << 22):
            yield chunk


def unpack(layout: Path, dest: Path) -> None:
    index = json.loads((layout / "index.json").read_text())
    [entry] = index["manifests"]
    manifest = json.loads(read_blob(layout, entry["digest"]))
    config = json.loads(read_blob(layout, manifest["config"]["digest"]))
    dest.mkdir(parents=True, exist_ok=True)
    for layer in manifest["layers"]:
        path = layout / "blobs/sha256" / layer["digest"].removeprefix("sha256:")
        extract(layer, file_chunks(path), dest)
        path.unlink()
    print(json.dumps(config))


class Unauthorized(urllib.request.HTTPRedirectHandler):
    """A blob redirects to its storage, whose own signed address refuses the registry's token."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        new = super().redirect_request(req, fp, code, msg, headers, newurl)
        if new is not None:
            new.remove_header("Authorization")
        return new


OPENER = urllib.request.build_opener(Unauthorized)


class Registry:
    """Anonymous pulls of one repository, by the token its challenge names."""

    def __init__(self, repository: str) -> None:
        self.registry, _, self.path = repository.partition("/")
        self.token = ""

    def open(self, kind: str, digest: str, accept: str = "*/*"):
        url = f"https://{self.registry}/v2/{self.path}/{kind}/{digest}"
        for attempt in (1, 2):
            headers = {"Accept": accept}
            if self.token:
                headers["Authorization"] = f"Bearer {self.token}"
            try:
                return OPENER.open(urllib.request.Request(url, headers=headers), timeout=120)
            except urllib.error.HTTPError as error:
                fields = dict(re.findall(r'(\w+)="([^"]*)"', error.headers.get("www-authenticate", "")))
                if error.code != 401 or attempt == 2 or "realm" not in fields:
                    die(f"{url}: {error}")
                query = urllib.parse.urlencode({k: v for k, v in fields.items() if k in ("service", "scope")})
                with urllib.request.urlopen(f"{fields['realm']}?{query}", timeout=60) as grant:
                    self.token = json.load(grant).get("token") or die("the registry granted no pull token")
        raise AssertionError("unreachable")

    def blob(self, digest: str) -> bytes:
        with self.open("blobs", digest) as response:
            data = response.read()
        if f"sha256:{hashlib.sha256(data).hexdigest()}" != digest:
            die(f"blob {digest} does not match its digest")
        return data


def fetch(image: str, dest: Path) -> None:
    repository, _, digest = image.partition("@")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        die(f"not an image by digest: {image}")
    registry = Registry(repository)
    with registry.open("manifests", digest, MANIFEST) as response:
        data = response.read()
    if f"sha256:{hashlib.sha256(data).hexdigest()}" != digest:
        die(f"the manifest of {image} does not match its digest")
    manifest = json.loads(data)
    config = json.loads(registry.blob(manifest["config"]["digest"]))
    dest.mkdir(parents=True, exist_ok=True)
    for layer in manifest["layers"]:
        with registry.open("blobs", layer["digest"]) as response:
            extract(layer, iter(lambda: response.read(1 << 22), b""), dest)
    print(json.dumps(config))


def read_blob(layout: Path, digest: str) -> bytes:
    data = (layout / "blobs/sha256" / digest.removeprefix("sha256:")).read_bytes()
    if f"sha256:{hashlib.sha256(data).hexdigest()}" != digest:
        die(f"blob {digest} does not match its digest")
    return data


def main(argv: list[str]) -> int:
    if len(argv) >= 4 and argv[1] == "pack":
        pack(Path(argv[2]), argv[3], argv[4:])
    elif len(argv) == 4 and argv[1] == "unpack":
        unpack(Path(argv[2]), Path(argv[3]))
    elif len(argv) == 4 and argv[1] == "fetch":
        fetch(argv[2], Path(argv[3]))
    else:
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
