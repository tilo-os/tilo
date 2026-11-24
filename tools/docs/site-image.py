#!/usr/bin/env python3
"""A static website's image: a base image from a registry, by digest, with one layer on top that
holds the site's files, written as an OCI layout a t check publishes (`publish = { layout }`).

    site-image.py OUT BASE SOURCE=PATH [SOURCE=PATH ...]

BASE is `registry/name@sha256:…`, pulled anonymously; an index is narrowed to its linux/amd64
image. Each SOURCE (a file or a directory) is put at PATH in the new layer. OUT gets `oci/` (the
layout) and `digest` (the image manifest's digest, what a deployment pins).

It reads and writes files and talks HTTPS to the registry: no container runtime, no root. The new
layer's entries are sorted, owned by root and readable by all, so the same files always give the
same layer, and a site whose files did not change gives the image it gave before. A file is dated
by its contents, not by when it was built: web servers validate caches by date and size (nginx's
ETag and Last-Modified), so a file that changes, even to the same size, gets another date.
"""

import gzip
import hashlib
import io
import json
import os
import re
import sys
import tarfile
import urllib.parse
import urllib.request

INDEX = "application/vnd.oci.image.index.v1+json"
MANIFEST = "application/vnd.oci.image.manifest.v1+json"
CONFIG = "application/vnd.oci.image.config.v1+json"
LAYER = "application/vnd.oci.image.layer.v1.tar+gzip"
DOCKER_LIST = "application/vnd.docker.distribution.manifest.list.v2+json"
DOCKER_MANIFEST = "application/vnd.docker.distribution.manifest.v2+json"
# Docker's media types name the same bytes as OCI's; the new manifest is OCI's throughout.
AS_OCI = {
    "application/vnd.docker.image.rootfs.diff.tar.gzip": LAYER,
    "application/vnd.docker.container.image.v1+json": CONFIG,
}


def die(message):
    print(f"site-image: {message}", file=sys.stderr)
    sys.exit(1)


class Registry:
    def __init__(self, reference):
        match = re.fullmatch(r"([a-z0-9.-]+(?::[0-9]+)?)/([a-z0-9._/-]+)@(sha256:[0-9a-f]{64})", reference)
        if not match:
            die(f"BASE must be registry/name@sha256:<digest>: {reference}")
        self.host, self.name, self.digest = match.groups()
        self.token = None

    def get(self, path, accept=None):
        url = f"https://{self.host}/v2/{self.name}/{path}"
        for _ in range(2):
            request = urllib.request.Request(url, headers={"Accept": accept} if accept else {})
            if self.token:
                # Not sent on to where a blob redirects (object storage refuses it).
                request.add_unredirected_header("Authorization", f"Bearer {self.token}")
            try:
                with urllib.request.urlopen(request, timeout=120) as response:
                    return response.read()
            except urllib.error.HTTPError as error:
                if error.code != 401 or self.token:
                    die(f"GET {url}: {error.code}")
                self.login(error.headers.get("WWW-Authenticate", ""))
        die(f"GET {url}: refused")

    def login(self, challenge):
        fields = dict(re.findall(r'(\w+)="([^"]*)"', challenge))
        if "realm" not in fields:
            die(f"{self.host} asks for credentials this anonymous pull has none of: {challenge}")
        query = {key: fields[key] for key in ("service", "scope") if key in fields}
        query.setdefault("scope", f"repository:{self.name}:pull")
        with urllib.request.urlopen(f"{fields['realm']}?{urllib.parse.urlencode(query)}", timeout=60) as response:
            answer = json.load(response)
        self.token = answer.get("token") or answer.get("access_token")

    def blob(self, digest):
        data = self.get(f"blobs/{digest}")
        if "sha256:" + hashlib.sha256(data).hexdigest() != digest:
            die(f"{self.host}/{self.name}: blob {digest} does not hold what its digest says")
        return data


def put(layout, data):
    digest = "sha256:" + hashlib.sha256(data).hexdigest()
    path = os.path.join(layout, "blobs", "sha256", digest.split(":")[1])
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)
    return digest


def base_manifest(registry):
    accept = ", ".join((INDEX, MANIFEST, DOCKER_LIST, DOCKER_MANIFEST))
    data = registry.get(f"manifests/{registry.digest}", accept)
    if "sha256:" + hashlib.sha256(data).hexdigest() != registry.digest:
        die(f"the registry's manifest is not {registry.digest}")
    doc = json.loads(data)
    if doc.get("mediaType") in (INDEX, DOCKER_LIST) or "manifests" in doc:
        chosen = [m for m in doc["manifests"]
                  if m.get("platform", {}).get("os") == "linux" and m.get("platform", {}).get("architecture") == "amd64"]
        if not chosen:
            die(f"{registry.digest} has no linux/amd64 image")
        data = registry.get(f"manifests/{chosen[0]['digest']}", ", ".join((MANIFEST, DOCKER_MANIFEST)))
        if "sha256:" + hashlib.sha256(data).hexdigest() != chosen[0]["digest"]:
            die(f"the registry's manifest is not {chosen[0]['digest']}")
        doc = json.loads(data)
    return doc


def dated(data):
    """A date for a file of `data`: the same contents, the same date; other contents, another."""
    return 1 + int.from_bytes(hashlib.sha256(data).digest()[:4], "big")


def add(tar, source, at):
    def entry(name, mode, kind, mtime=0):
        info = tarfile.TarInfo(name)
        info.mode, info.type, info.mtime, info.uid, info.gid, info.uname, info.gname = mode, kind, mtime, 0, 0, "", ""
        return info

    parts = at.strip("/").split("/")
    for i in range(1, len(parts)):
        tar.addfile(entry("/".join(parts[:i]), 0o755, tarfile.DIRTYPE))
    if os.path.isfile(source):
        with open(source, "rb") as f:
            data = f.read()
        info = entry(at.strip("/"), 0o644, tarfile.REGTYPE, dated(data))
        info.size = len(data)
        tar.addfile(info, io.BytesIO(data))
        return
    if not os.path.isdir(source):
        die(f"{source} is neither a file nor a directory")
    for directory, subdirectories, files in os.walk(source):
        subdirectories.sort()
        relative = os.path.relpath(directory, source)
        name = at.strip("/") if relative == "." else f"{at.strip('/')}/{relative}"
        tar.addfile(entry(name, 0o755, tarfile.DIRTYPE))
        for file in sorted(files):
            path = os.path.join(directory, file)
            if os.path.islink(path):
                die(f"{path} is a symlink: a site's files are its own")
            with open(path, "rb") as f:
                data = f.read()
            info = entry(f"{name}/{file}", 0o644, tarfile.REGTYPE, dated(data))
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))


def main(argv):
    if len(argv) < 3:
        die("usage: site-image.py OUT BASE SOURCE=PATH [SOURCE=PATH ...]")
    out, base, mappings = argv[0], argv[1], argv[2:]
    layout = os.path.join(out, "oci")
    os.makedirs(layout, exist_ok=True)
    registry = Registry(base)
    manifest = base_manifest(registry)
    config = json.loads(registry.blob(manifest["config"]["digest"]))
    layers = []
    for layer in manifest["layers"]:
        digest = put(layout, registry.blob(layer["digest"]))
        layers.append({"mediaType": AS_OCI.get(layer["mediaType"], layer["mediaType"]),
                       "digest": digest, "size": layer["size"]})

    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as tar:
        for mapping in mappings:
            source, _, at = mapping.partition("=")
            if not at.startswith("/"):
                die(f"{mapping}: give SOURCE=/absolute/path")
            add(tar, source, at)
    uncompressed = raw.getvalue()
    compressed = io.BytesIO()
    with gzip.GzipFile(fileobj=compressed, mode="wb", mtime=0, compresslevel=9) as gz:
        gz.write(uncompressed)
    layer = compressed.getvalue()
    layers.append({"mediaType": LAYER, "digest": put(layout, layer), "size": len(layer)})
    config["rootfs"]["diff_ids"].append("sha256:" + hashlib.sha256(uncompressed).hexdigest())
    config.setdefault("history", []).append({"created_by": "site-image.py: the site's files"})
    config_bytes = json.dumps(config, sort_keys=True, separators=(",", ":")).encode()
    image = {
        "schemaVersion": 2,
        "mediaType": MANIFEST,
        "config": {"mediaType": CONFIG, "digest": put(layout, config_bytes), "size": len(config_bytes)},
        "layers": layers,
    }
    image_bytes = json.dumps(image, sort_keys=True, separators=(",", ":")).encode()
    digest = put(layout, image_bytes)
    index = {"schemaVersion": 2, "mediaType": INDEX,
             "manifests": [{"mediaType": MANIFEST, "digest": digest, "size": len(image_bytes),
                            "platform": {"architecture": "amd64", "os": "linux"}}]}
    with open(os.path.join(layout, "index.json"), "w") as f:
        json.dump(index, f, sort_keys=True)
    with open(os.path.join(layout, "oci-layout"), "w") as f:
        json.dump({"imageLayoutVersion": "1.0.0"}, f)
    with open(os.path.join(out, "digest"), "w") as f:
        f.write(digest + "\n")
    print(digest)


if __name__ == "__main__":
    main(sys.argv[1:])
