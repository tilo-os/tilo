#!/usr/bin/env python3
"""Finish a chunkah re-layering of the OS image and prove it is the same tree.

os/Containerfile.rechunk writes the raw image's root again as content-based
layers (TIL-747). Before that layout is published, this script

1. replays the raw image's layers (whiteouts and opaque directories applied)
   and the chunked layers, hashing every regular file;
2. gives every entry the raw image's extended attributes where chunkah's
   differ, rewriting only the layers that hold one. chunkah reads the root
   through BuildKit's snapshot of the image, and that snapshot lacks the file
   capabilities of the base image's binaries (arping, newuidmap, the sssd
   helpers...) that the image's layers carry: the layers, not the snapshot,
   are the image;
3. appends one small layer holding every directory of the raw image with its
   exact mode, owner, time and extended attributes. chunkah writes a
   directory into each layer that holds something below it, its time capped
   at that layer's clamp, and the top-most copy wins; fontconfig's system
   cache, among others, is valid only while a font directory's time is the one
   `fc-cache` recorded, so directory times must come through unchanged;
4. compares the two roots. Every path must have the same type, mode, owner,
   extended attributes, size and bytes, symlink target and device numbers.
   Two differences are expected and allowed, and counted in the report:
   * a file's or symlink's time may be lower (chunkah caps it at its
     package's build time, or the epoch for files no package owns);
   * the contents of /sysroot are gone (the base's ostree repository, which
     bootc's composefs backend does not deploy), and a hard-link group may be
     split where its paths fall into different layers (same bytes, read-only).
   Anything else fails: a rechunk that cannot be proven equal is not published;
5. gives the chunked image the raw image's config (labels, environment,
   command, creation time) and names it in index.json the way buildx does, so
   os/scripts/oci-archive-ref.py and skopeo's oci and oci-archive transports
   select it; optionally writes it as an OCI archive.

    oci-rechunk.py finish --raw registry.tilo.so/tilo/tilo-ci@sha256:... \\
        --chunked out/image --name registry.tilo.so/tilo/tilo-ci:TAG \\
        [--archive /var/tmp/tilo-image.tar] [--report report.json]

`--raw` is a registry reference or `oci:DIR` (an OCI layout). The manifest
digest of the finished image is the last line of standard output.
"""

from __future__ import annotations

import argparse
import base64
import concurrent.futures
import copy
import dataclasses
import decimal
import gzip
import hashlib
import io
import json
import os
import sys
import tarfile
import tempfile
import time
from pathlib import Path
from typing import Any, Iterable

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools" / "perf"))

import image_size  # noqa: E402

LAYER_GZIP = "application/vnd.oci.image.layer.v1.tar+gzip"
MANIFEST = "application/vnd.oci.image.manifest.v1+json"
INDEX = "application/vnd.oci.image.index.v1+json"
CONFIG = "application/vnd.oci.image.config.v1+json"
# chunkah --prune /sysroot/: its contents go, the directory stays.
PRUNED = "sysroot/"
# Labels that describe the base's ostree commit, which the chunked image no
# longer carries (chunkah's own guidance for a pruned /sysroot).
DROPPED_LABELS = ("ostree.commit", "ostree.final-diffid")
LAYERING_LABEL = "org.tilo.layering"
DIRECTORY_LAYER = "oci-rechunk: every directory's mode, owner, time and xattrs, from the raw image (TIL-747)"
SHOWN = 40


# ---------------------------------------------------------------- the tree


@dataclasses.dataclass(frozen=True)
class Node:
    kind: str  # dir, file, symlink, char, block, fifo
    mode: int
    uid: int
    gid: int
    mtime: decimal.Decimal
    xattrs: tuple[tuple[str, bytes], ...]
    size: int = 0
    sha: str = ""
    link: str = ""
    device: tuple[int, int] = (0, 0)
    # The inode a hard-link group shares; every file and symlink has its own.
    inode: int = 0
    # A parent directory no layer has an entry for: its metadata is unknown.
    implicit: bool = False


KINDS = {tarfile.DIRTYPE: "dir", tarfile.REGTYPE: "file", tarfile.AREGTYPE: "file",
         tarfile.CONTTYPE: "file", tarfile.SYMTYPE: "symlink", tarfile.CHRTYPE: "char",
         tarfile.BLKTYPE: "block", tarfile.FIFOTYPE: "fifo"}


def normal(name: str) -> str:
    return image_size.normal(name)


def xattrs(member: tarfile.TarInfo) -> tuple[tuple[str, bytes], ...]:
    found = {}
    for key, value in member.pax_headers.items():
        if key.startswith("SCHILY.xattr."):
            found[key[len("SCHILY.xattr."):]] = value.encode("utf-8", "surrogateescape")
        elif key.startswith("LIBARCHIVE.xattr."):
            found[key[len("LIBARCHIVE.xattr."):]] = base64.b64decode(value)
    return tuple(sorted(found.items()))


def mtime(member: tarfile.TarInfo) -> decimal.Decimal:
    exact = member.pax_headers.get("mtime")
    return decimal.Decimal(exact) if exact is not None else decimal.Decimal(int(member.mtime))


class Tree:
    """A merged root as layers replay, with each path's full metadata."""

    def __init__(self, record: bool = False) -> None:
        self.nodes: dict[str, Node] = {}
        self.layer: dict[str, int] = {}
        self.inodes = 0
        # With `record`, every path each layer writes an entry for, by layer.
        self.members: dict[int, set[str]] | None = {} if record else None

    def below(self, path: str) -> list[str]:
        prefix = path + "/" if path else ""
        return [name for name in self.nodes if name.startswith(prefix)]

    def drop(self, path: str) -> None:
        """Remove `path`, and everything below it when it is a directory."""
        node = self.nodes.pop(path, None)
        self.layer.pop(path, None)
        if node is not None and node.kind == "dir":
            for name in self.below(path):
                del self.nodes[name]
                self.layer.pop(name, None)

    def parents(self, path: str, index: int) -> None:
        """Directories a layer implies by naming something below them."""
        parent = path.rpartition("/")[0]
        while parent and parent not in self.nodes:
            self.nodes[parent] = Node("dir", 0o755, 0, 0, decimal.Decimal(0), (), implicit=True)
            self.layer[parent] = index
            parent = parent.rpartition("/")[0]

    def apply(self, index: int, archive: tarfile.TarFile) -> None:
        for member in archive:
            path = normal(member.name)
            if not path:
                continue  # the root's own entry; no image is compared on it
            parent, _, base = path.rpartition("/")
            if base == ".wh..wh..opq":
                for name in self.below(parent):
                    if self.layer.get(name, index) < index:
                        self.nodes.pop(name, None)
                        self.layer.pop(name, None)
                continue
            if base.startswith(".wh."):
                self.drop(f"{parent}/{base[4:]}" if parent else base[4:])
                continue
            if self.members is not None:
                self.members.setdefault(index, set()).add(path)
            existing = self.nodes.get(path)
            if existing is not None and (existing.kind != "dir" or not member.isdir()):
                self.drop(path)
            self.parents(path, index)
            if member.islnk():
                target = self.nodes.get(normal(member.linkname))
                if target is None or target.kind == "dir":
                    raise ValueError(f"layer {index}: {path} links to {member.linkname}, which is not a file")
                self.nodes[path] = target
                self.layer[path] = index
                continue
            kind = KINDS.get(member.type)
            if kind is None:
                raise ValueError(f"layer {index}: {path} has unsupported tar type {member.type!r}")
            size, sha = 0, ""
            if kind == "file":
                stream = archive.extractfile(member)
                assert stream is not None
                digest = hashlib.sha256()
                while chunk := stream.read(image_size.CHUNK):
                    digest.update(chunk)
                size, sha = member.size, digest.hexdigest()
            inode = 0
            if kind != "dir":
                self.inodes += 1
                inode = self.inodes
            self.nodes[path] = Node(
                kind, member.mode & 0o7777, member.uid, member.gid, mtime(member), xattrs(member),
                size=size, sha=sha, link=member.linkname if kind == "symlink" else "",
                device=(member.devmajor, member.devminor) if kind in ("char", "block") else (0, 0),
                inode=inode)
            self.layer[path] = index


def replay(image: image_size.Image, tree: Tree | None = None, start: int = 0) -> Tree:
    """Every layer of `image`, from `start`, each blob checked against its digest."""
    tree = tree or Tree()
    layers = image.manifest.get("layers", [])
    for index, item in enumerate(layers[start:], start):
        digest = item["digest"]
        if not image_size.SHA256.fullmatch(digest):
            raise ValueError(f"invalid layer digest {digest}")
        with image.blob(digest) as raw:
            verified = image_size.Verified(raw)
            stream = image_size.decompressed(item["mediaType"], io.BufferedReader(verified, image_size.CHUNK))
            with tarfile.open(fileobj=stream, mode="r|") as archive:
                tree.apply(index, archive)
            verified.drain()
        if verified.digest != digest or verified.count != int(item["size"]):
            raise ValueError(f"layer {digest} fails its digest check ({verified.count} bytes read)")
    return tree


def open_image(source: str) -> image_size.Image:
    if source.startswith("oci:"):
        return image_size.layout_image(Path(source[4:]), None)
    return image_size.registry_image(source, None)


def replay_source(source: str, record: bool = False) -> Tree:
    return replay(open_image(source), Tree(record))


# ---------------------------------------------------------------- extended attributes

XATTR_KEYS = ("SCHILY.xattr.", "LIBARCHIVE.xattr.")


def xattr_repairs(raw: Tree, chunked: Tree) -> dict[str, tuple[tuple[str, bytes], ...]]:
    """Paths whose extended attributes chunkah wrote differently, mapped to the raw image's."""
    repairs = {}
    for path, node in chunked.nodes.items():
        before = raw.nodes.get(path)
        if (before is None or before.implicit or before.kind != node.kind or path.startswith(PRUNED)
                or before.xattrs == node.xattrs):
            continue
        repairs[path] = before.xattrs
    return repairs


def rewrite_layer(root: Path, item: dict[str, Any],
                  repairs: dict[str, tuple[tuple[str, bytes], ...]]) -> tuple[dict[str, Any], str, int]:
    """One chunkah layer again with the raw image's xattrs on `repairs`: its descriptor, diff ID and count."""
    blobs = root / "blobs" / "sha256"
    plain, packed = root / "rewrite.tar", root / "rewrite.tar.gz"
    rewritten = 0
    try:
        with (blobs / item["digest"][7:]).open("rb") as source, plain.open("wb") as sink:
            stream = image_size.decompressed(item["mediaType"], source)
            with tarfile.open(fileobj=stream, mode="r|") as archive, \
                    tarfile.open(fileobj=sink, mode="w", format=tarfile.PAX_FORMAT) as output:
                for member in archive:
                    wanted = repairs.get(normal(member.name))
                    if wanted is not None:
                        headers = {key: value for key, value in member.pax_headers.items()
                                   if not key.startswith(XATTR_KEYS)}
                        headers.update({f"SCHILY.xattr.{key}": value.decode("utf-8", "surrogateescape")
                                        for key, value in wanted})
                        member.pax_headers = headers
                        rewritten += 1
                    output.addfile(member, archive.extractfile(member) if member.isreg() else None)
        diff = hashlib.sha256()
        with plain.open("rb") as source, packed.open("wb") as sink, \
                gzip.GzipFile(filename="", mode="wb", fileobj=sink, compresslevel=6, mtime=0) as packer:
            while chunk := source.read(image_size.CHUNK):
                diff.update(chunk)
                packer.write(chunk)
        digest = hashlib.sha256()
        with packed.open("rb") as source:
            while chunk := source.read(image_size.CHUNK):
                digest.update(chunk)
        target = blobs / digest.hexdigest()
        os.replace(packed, target)
        return ({**item, "digest": "sha256:" + target.name, "size": target.stat().st_size},
                "sha256:" + diff.hexdigest(), rewritten)
    finally:
        plain.unlink(missing_ok=True)
        packed.unlink(missing_ok=True)


def repair_xattrs(root: Path, chunked: Tree, repairs: dict[str, tuple[tuple[str, bytes], ...]]) -> tuple[int, int]:
    """Rewrite every layer of the chunkah layout that writes a path in `repairs`; return layers and entries."""
    assert chunked.members is not None
    image = image_size.layout_image(root, None)
    manifest, config = copy.deepcopy(image.manifest), copy.deepcopy(image.config)
    affected = sorted(index for index, paths in chunked.members.items() if not paths.isdisjoint(repairs))
    entries = 0
    for index in affected:
        manifest["layers"][index], config["rootfs"]["diff_ids"][index], count = rewrite_layer(
            root, manifest["layers"][index], repairs)
        entries += count
    config_bytes = canonical(config)
    manifest["config"] = {**manifest["config"], "digest": put(root, config_bytes), "size": len(config_bytes)}
    manifest_bytes = canonical(manifest)
    index = json.loads((root / "index.json").read_text(encoding="utf-8"))
    index["manifests"] = [{**index["manifests"][0], "digest": put(root, manifest_bytes), "size": len(manifest_bytes)}]
    (root / "index.json").write_bytes(canonical(index))
    return len(affected), entries


# ---------------------------------------------------------------- the directory layer


def directory_layer(tree: Tree) -> bytes:
    """An uncompressed tar of every directory with its exact metadata, parents first."""
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for path in sorted(name for name, node in tree.nodes.items() if node.kind == "dir"):
            node = tree.nodes[path]
            if path.startswith(PRUNED) or node.implicit:
                continue
            member = tarfile.TarInfo(path)
            member.type = tarfile.DIRTYPE
            member.mode, member.uid, member.gid = node.mode, node.uid, node.gid
            member.uname = member.gname = ""
            member.mtime = int(node.mtime)
            headers = {f"SCHILY.xattr.{key}": value.decode("utf-8", "surrogateescape")
                       for key, value in node.xattrs}
            if node.mtime != int(node.mtime):
                headers["mtime"] = str(node.mtime)
            member.pax_headers = headers
            archive.addfile(member)
    return output.getvalue()


def gzipped(data: bytes) -> bytes:
    output = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=output, compresslevel=6, mtime=0) as stream:
        stream.write(data)
    return output.getvalue()


# ---------------------------------------------------------------- the proof


@dataclasses.dataclass
class Comparison:
    problems: list[str] = dataclasses.field(default_factory=list)
    paths: int = 0
    clamped: int = 0
    pruned: int = 0
    pruned_bytes: int = 0
    split_links: int = 0

    def fail(self, text: str) -> None:
        self.problems.append(text)


def groups(tree: Tree, paths: Iterable[str]) -> dict[int, list[str]]:
    found: dict[int, list[str]] = {}
    for path in paths:
        node = tree.nodes[path]
        if node.inode:
            found.setdefault(node.inode, []).append(path)
    return {inode: sorted(names) for inode, names in found.items() if len(names) > 1}


def directory_drift(raw: Tree, chunked: Tree) -> tuple[list[str], int]:
    """Directories as chunkah wrote them, before the directory layer: only their times may differ.

    The directory layer restores times. It must not be what hides a changed
    mode, owner or extended attribute: how an upper layer's directory entry
    merges with a lower one's xattrs differs between overlay stores, so the
    lower entries have to be right already. Returns the problems and the
    number of directories whose time the layer restores.
    """
    problems, restored = [], 0
    for path, node in chunked.nodes.items():
        before = raw.nodes.get(path)
        if node.kind != "dir" or before is None or before.kind != "dir" or before.implicit:
            continue
        for field in ("mode", "uid", "gid", "xattrs"):
            wrote, has = getattr(node, field), getattr(before, field)
            if wrote != has:
                shown = (oct(wrote), oct(has)) if field == "mode" else (repr(wrote), repr(has))
                problems.append(f"/{path}: chunkah wrote the directory's {field} {shown[0]}, "
                                f"the raw image has {shown[1]}")
        restored += before.mtime != node.mtime
    return sorted(problems), restored


def compare(raw: Tree, chunked: Tree) -> Comparison:
    result = Comparison()
    kept = {path for path in raw.nodes if not path.startswith(PRUNED)}
    # Bytes only /sysroot names, once per inode (image_size.py's `undeployed`):
    # the base's ostree objects hard-linked to /usr stay in the image.
    deployed = {raw.nodes[path].inode for path in kept}
    counted = set()
    for path, node in raw.nodes.items():
        if path.startswith(PRUNED):
            result.pruned += 1
            if node.inode not in deployed and node.inode not in counted:
                counted.add(node.inode)
                result.pruned_bytes += node.size
    present = set(chunked.nodes)
    for path in sorted(kept - present):
        result.fail(f"missing: /{path}")
    for path in sorted(present - kept):
        result.fail(f"not in the raw image: /{path}")
    for path in sorted(kept & present):
        result.paths += 1
        a, b = raw.nodes[path], chunked.nodes[path]
        if a.implicit and b.kind == "dir":
            continue  # no layer of the raw image says what this directory's metadata is
        for field in ("kind", "mode", "uid", "gid", "xattrs", "size", "sha", "link", "device"):
            was, now = getattr(a, field), getattr(b, field)
            if was != now:
                shown = (oct(was), oct(now)) if field == "mode" else (repr(was), repr(now))
                result.fail(f"/{path}: {field} {shown[0]} became {shown[1]}")
        if a.kind == "dir":
            if a.mtime != b.mtime:
                result.fail(f"/{path}: directory time {a.mtime} became {b.mtime}")
        elif b.mtime > a.mtime:
            result.fail(f"/{path}: time {a.mtime} rose to {b.mtime}")
        elif b.mtime < a.mtime:
            result.clamped += 1
    common = sorted(kept & present)
    raw_inode = {path: raw.nodes[path].inode for path in common}
    for names in groups(chunked, common).values():
        if len({raw_inode[name] for name in names}) != 1:
            result.fail(f"hard-linked in the chunked image but not in the raw one: {', '.join('/' + n for n in names)}")
    for names in groups(raw, common).values():
        if len({chunked.nodes[name].inode for name in names}) != 1:
            result.split_links += 1
    return result


# ---------------------------------------------------------------- the layout


def canonical(document: dict[str, Any]) -> bytes:
    return json.dumps(document, separators=(",", ":")).encode()


def put(root: Path, data: bytes) -> str:
    digest = "sha256:" + hashlib.sha256(data).hexdigest()
    path = root / "blobs" / "sha256" / digest[7:]
    if not path.exists():
        path.write_bytes(data)
    return digest


def adopt_config(chunked: dict[str, Any], raw: dict[str, Any], diff_id: str, layers: int) -> dict[str, Any]:
    """chunkah's rootfs and history under the raw image's config."""
    if chunked.get("architecture") != raw.get("architecture") or chunked.get("os") != raw.get("os"):
        raise ValueError(f"chunkah wrote {chunked.get('os')}/{chunked.get('architecture')}, "
                         f"the raw image is {raw.get('os')}/{raw.get('architecture')}")
    config = copy.deepcopy(chunked)
    settings = copy.deepcopy(raw.get("config") or {})
    labels = {key: value for key, value in (settings.get("Labels") or {}).items() if key not in DROPPED_LABELS}
    labels[LAYERING_LABEL] = f"chunkah {layers} layers"
    settings["Labels"] = labels
    config["config"] = settings
    for key in ("created", "author", "variant", "os.version", "os.features"):
        if key in raw:
            config[key] = raw[key]
    config.setdefault("rootfs", {"type": "layers", "diff_ids": []})["diff_ids"].append(diff_id)
    history = config.setdefault("history", [])
    history.append({"created": raw.get("created", ""), "created_by": DIRECTORY_LAYER})
    made = [entry for entry in history if not entry.get("empty_layer")]
    if len(made) != len(config["rootfs"]["diff_ids"]):
        raise ValueError(f"{len(made)} history entries for {len(config['rootfs']['diff_ids'])} layers")
    return config


def finish_layout(root: Path, raw_config: dict[str, Any], name: str, layer: bytes) -> tuple[str, dict[str, Any]]:
    """Add the directory layer and the raw config to the chunkah layout; return the manifest digest."""
    index = json.loads((root / "index.json").read_text(encoding="utf-8"))
    if len(index.get("manifests", [])) != 1:
        raise ValueError("chunkah's layout must hold exactly one manifest")
    image = image_size.layout_image(root, None)
    manifest = copy.deepcopy(image.manifest)
    compressed = gzipped(layer)
    diff_id = "sha256:" + hashlib.sha256(layer).hexdigest()
    descriptor = {"mediaType": LAYER_GZIP, "digest": put(root, compressed), "size": len(compressed)}
    manifest["layers"] = [*manifest["layers"], descriptor]
    config = adopt_config(image.config, raw_config, diff_id, len(manifest["layers"]))
    config_bytes = canonical(config)
    manifest["config"] = {"mediaType": CONFIG, "digest": put(root, config_bytes), "size": len(config_bytes)}
    manifest.setdefault("mediaType", MANIFEST)
    manifest_bytes = canonical(manifest)
    digest = put(root, manifest_bytes)
    tag = name.rsplit(":", 1)[1] if ":" in name.rsplit("/", 1)[-1] else ""
    if not tag:
        raise ValueError(f"{name}: name the image with a tag")
    annotations = {"io.containerd.image.name": name, "org.opencontainers.image.ref.name": tag}
    (root / "index.json").write_bytes(canonical({
        "schemaVersion": 2, "mediaType": INDEX,
        "manifests": [{"mediaType": MANIFEST, "digest": digest, "size": len(manifest_bytes),
                       "annotations": annotations}]}))
    wanted = {digest[7:], manifest["config"]["digest"][7:], *(item["digest"][7:] for item in manifest["layers"])}
    for blob in (root / "blobs" / "sha256").iterdir():
        if blob.name not in wanted:
            blob.unlink()
    return digest, manifest


def write_archive(root: Path, destination: Path) -> None:
    """The layout as an uncompressed OCI archive, written beside `destination` and moved over it."""
    def member(name: str, size: int = 0, directory: bool = False) -> tarfile.TarInfo:
        info = tarfile.TarInfo(name)
        info.type = tarfile.DIRTYPE if directory else tarfile.REGTYPE
        info.mode, info.size, info.mtime = (0o755 if directory else 0o644), size, 0
        info.uid = info.gid = 0
        info.uname = info.gname = ""
        return info

    handle, temporary = tempfile.mkstemp(prefix=destination.name + ".", dir=destination.parent)
    os.close(handle)
    try:
        with tarfile.open(temporary, mode="w", format=tarfile.PAX_FORMAT) as archive:
            for name in ("oci-layout", "index.json"):
                data = (root / name).read_bytes()
                archive.addfile(member(name, len(data)), io.BytesIO(data))
            archive.addfile(member("blobs", directory=True))
            archive.addfile(member("blobs/sha256", directory=True))
            for blob in sorted((root / "blobs" / "sha256").iterdir()):
                with blob.open("rb") as stream:
                    archive.addfile(member(f"blobs/sha256/{blob.name}", blob.stat().st_size), stream)
        os.replace(temporary, destination)
    except BaseException:
        Path(temporary).unlink(missing_ok=True)
        raise


# ---------------------------------------------------------------- finish


def finish(raw_source: str, chunked: Path, name: str, archive: Path | None,
           report_path: Path | None) -> str:
    started = time.monotonic()
    raw_image = open_image(raw_source)
    before = image_size.layout_image(chunked, None)
    with concurrent.futures.ProcessPoolExecutor(max_workers=2) as pool:
        raw_future = pool.submit(replay_source, raw_source)
        chunked_future = pool.submit(replay_source, f"oci:{chunked}", True)
        raw_tree, chunked_tree = raw_future.result(), chunked_future.result()
    repairs = xattr_repairs(raw_tree, chunked_tree)
    repaired_layers = repaired_entries = 0
    if repairs:
        repaired_layers, repaired_entries = repair_xattrs(chunked, chunked_tree, repairs)
        # Everything below is proven on the layers as rewritten.
        chunked_tree = replay_source(f"oci:{chunked}")
    drift, restored = directory_drift(raw_tree, chunked_tree)
    layer = directory_layer(raw_tree)
    digest, manifest = finish_layout(chunked, raw_image.config, name, layer)
    # The proof runs on the layout as written: chunkah's layers, then the
    # directory layer read back from its blob.
    final = image_size.layout_image(chunked, None)
    if final.manifest_digest != digest:
        raise ValueError(f"the layout names {final.manifest_digest}, not {digest}")
    replay(final, chunked_tree, start=len(manifest["layers"]) - 1)
    result = compare(raw_tree, chunked_tree)
    result.problems[:0] = drift
    report = {
        "raw": raw_image.reference, "raw_layers": len(raw_image.manifest.get("layers", [])),
        "raw_compressed": sum(int(item["size"]) for item in raw_image.manifest.get("layers", [])),
        "chunked": f"{name}@{digest}", "chunked_layers": len(manifest["layers"]),
        "chunked_compressed": sum(int(item["size"]) for item in manifest["layers"]),
        "directory_layer": {"compressed": manifest["layers"][-1]["size"], "uncompressed": len(layer)},
        "paths": result.paths, "clamped_times": result.clamped, "pruned_paths": result.pruned,
        "pruned_bytes": result.pruned_bytes, "split_hard_link_groups": result.split_links,
        "directory_times_restored": restored,
        "xattrs_restored": {"paths": len(repairs), "entries": repaired_entries, "layers": repaired_layers,
                            "first": sorted("/" + path for path in repairs)[:SHOWN]},
        "problems": len(result.problems), "seconds": round(time.monotonic() - started, 1),
        "before": before.manifest_digest,
    }
    if report_path:
        report_path.write_text(json.dumps(report, indent=1) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=1), file=sys.stderr)
    if result.problems:
        for problem in result.problems[:SHOWN]:
            print(f"oci-rechunk: {problem}", file=sys.stderr)
        raise ValueError(f"the chunked image differs from the raw image in {len(result.problems)} ways "
                         f"(first {min(SHOWN, len(result.problems))} above); not publishing it")
    if archive:
        write_archive(chunked, archive)
    return digest


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    command = commands.add_parser("finish", help="add the directory layer, adopt the config, prove equality")
    command.add_argument("--raw", required=True, help="the raw image: a registry reference or oci:DIR")
    command.add_argument("--chunked", required=True, type=Path, help="chunkah's OCI layout, finished in place")
    command.add_argument("--name", required=True, help="the image's full name with its tag")
    command.add_argument("--archive", type=Path, help="also write the finished image as this OCI archive")
    command.add_argument("--report", type=Path, help="write the comparison report as JSON")
    args = parser.parse_args(argv)
    try:
        print(finish(args.raw, args.chunked, args.name, args.archive, args.report))
    except (OSError, ValueError, KeyError, tarfile.TarError) as error:
        print(f"oci-rechunk: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
