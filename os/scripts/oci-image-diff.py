#!/usr/bin/env python3
"""Explain why two OCI images that should be identical are not.

Each image is an OCI layout directory or an OCI archive (a tar of one),
optionally followed by `@sha256:<manifest digest>` to pick one manifest from
an index that holds several. Prints the config differences, a layer-by-layer
digest comparison next to the instruction that produced each layer, and, for
the first differing layers, the first files whose metadata or content differ.

Exits 0 when the manifests are byte-identical, 1 when they differ.
"""

from __future__ import annotations

import argparse
import contextlib
import difflib
import gzip
import hashlib
import io
import json
import os
import sys
import tarfile
from typing import IO, Iterator

LAYER_MEDIA_GZIP = (
    "application/vnd.oci.image.layer.v1.tar+gzip",
    "application/vnd.docker.image.rootfs.diff.tar.gzip",
)
LAYER_MEDIA_ZSTD = ("application/vnd.oci.image.layer.v1.tar+zstd",)
INDEX_MEDIA = (
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
)
TEXT_LIMIT = 256 * 1024


class Layout:
    """Blob access for an OCI layout directory or an OCI archive."""

    def __init__(self, spec: str):
        path, _, digest = spec.partition("@")
        self.name = spec
        self.path = path
        self.pinned = digest or None
        self.members: dict[str, tarfile.TarInfo] | None = None
        self.tar: tarfile.TarFile | None = None
        if os.path.isdir(path):
            return
        self.tar = tarfile.open(path, "r:")
        self.members = {}
        for member in self.tar:
            self.members[member.name.removeprefix("./")] = member

    def close(self) -> None:
        if self.tar is not None:
            self.tar.close()

    def open(self, name: str) -> IO[bytes]:
        if self.tar is None:
            return open(os.path.join(self.path, name), "rb")
        assert self.members is not None
        member = self.members.get(name)
        handle = self.tar.extractfile(member) if member else None
        if handle is None:
            raise FileNotFoundError(f"{self.path}: no {name}")
        return handle

    def blob(self, digest: str) -> IO[bytes]:
        algorithm, _, value = digest.partition(":")
        return self.open(f"blobs/{algorithm}/{value}")

    def read_blob(self, digest: str) -> bytes:
        with self.blob(digest) as handle:
            data = handle.read()
        if hashlib.sha256(data).hexdigest() != digest.partition(":")[2]:
            raise ValueError(f"{self.path}: blob {digest} does not match its digest")
        return data

    def manifest(self) -> tuple[str, bytes]:
        if self.pinned:
            return self.pinned, self.read_blob(self.pinned)
        with self.open("index.json") as handle:
            index = json.load(handle)
        while True:
            descriptors = index.get("manifests", [])
            digests = sorted({item["digest"] for item in descriptors})
            if len(digests) != 1:
                raise ValueError(
                    f"{self.path}: index names {len(digests)} manifests; append @<digest> to pick one"
                )
            data = self.read_blob(digests[0])
            parsed = json.loads(data)
            if parsed.get("mediaType") in INDEX_MEDIA or "manifests" in parsed:
                index = parsed
                continue
            return digests[0], data


def short(digest: str) -> str:
    return digest.partition(":")[2][:12]


def layer_history(config: dict) -> list[str]:
    """The history entry that produced each layer, in layer order."""
    produced = []
    for entry in config.get("history", []):
        if entry.get("empty_layer"):
            continue
        produced.append(" ".join(entry.get("created_by", "").split()))
    return produced


def compare_configs(config_a: dict, config_b: dict) -> list[str]:
    lines = []
    for key in sorted(set(config_a) | set(config_b)):
        if key in ("rootfs", "history"):
            continue
        if config_a.get(key) != config_b.get(key):
            lines.append(f"  config.{key}: {json.dumps(config_a.get(key))[:300]} != {json.dumps(config_b.get(key))[:300]}")
    history_a = config_a.get("history", [])
    history_b = config_b.get("history", [])
    if len(history_a) != len(history_b):
        lines.append(f"  history length: {len(history_a)} != {len(history_b)}")
    for position, (entry_a, entry_b) in enumerate(zip(history_a, history_b)):
        if entry_a != entry_b:
            lines.append(f"  history[{position}]: {json.dumps(entry_a)[:300]} != {json.dumps(entry_b)[:300]}")
    diff_ids_a = config_a.get("rootfs", {}).get("diff_ids", [])
    diff_ids_b = config_b.get("rootfs", {}).get("diff_ids", [])
    differing = sum(1 for a, b in zip(diff_ids_a, diff_ids_b) if a != b) + abs(len(diff_ids_a) - len(diff_ids_b))
    if differing:
        lines.append(f"  rootfs.diff_ids: {differing} of {max(len(diff_ids_a), len(diff_ids_b))} differ")
    return lines


@contextlib.contextmanager
def open_layer(layout: Layout, descriptor: dict) -> Iterator[IO[bytes]]:
    with layout.blob(descriptor["digest"]) as raw:
        media = descriptor.get("mediaType", "")
        if media in LAYER_MEDIA_GZIP:
            with gzip.GzipFile(fileobj=raw) as stream:
                yield stream
        elif media in LAYER_MEDIA_ZSTD:
            import zstandard  # type: ignore[import-not-found]

            with zstandard.ZstdDecompressor().stream_reader(raw) as stream:
                yield stream
        else:
            yield raw


class Entry:
    __slots__ = ("kind", "mode", "owner", "mtime", "size", "link", "pax", "sha256", "text")

    def __init__(self, member: tarfile.TarInfo, content: bytes | None, sha256: str | None):
        self.kind = member.type.decode(errors="replace") if isinstance(member.type, bytes) else str(member.type)
        self.mode = oct(member.mode)
        self.owner = f"{member.uid}:{member.gid}"
        self.mtime = int(member.mtime)
        self.size = member.size
        self.link = member.linkname
        self.pax = {key: value for key, value in sorted(member.pax_headers.items()) if key not in ("path", "linkpath", "size", "mtime")}
        self.sha256 = sha256
        self.text = content

    def fields(self) -> dict[str, object]:
        return {
            "type": self.kind,
            "mode": self.mode,
            "owner": self.owner,
            "mtime": self.mtime,
            "size": self.size,
            "link": self.link,
            "pax": self.pax,
            "content": self.sha256,
        }


def layer_entries(layout: Layout, descriptor: dict) -> tuple[list[str], dict[str, Entry]]:
    order: list[str] = []
    entries: dict[str, Entry] = {}
    with open_layer(layout, descriptor) as stream:
        with tarfile.open(fileobj=stream, mode="r|") as archive:
            for member in archive:
                content = None
                digest = None
                if member.isreg():
                    handle = archive.extractfile(member)
                    hasher = hashlib.sha256()
                    kept = io.BytesIO() if member.size <= TEXT_LIMIT else None
                    assert handle is not None
                    while chunk := handle.read(1 << 20):
                        hasher.update(chunk)
                        if kept is not None:
                            kept.write(chunk)
                    digest = hasher.hexdigest()
                    content = kept.getvalue() if kept is not None else None
                name = member.name.removeprefix("./")
                order.append(name)
                entries[name] = Entry(member, content, digest)
    return order, entries


def text_diff(name: str, left: bytes | None, right: bytes | None, limit: int) -> list[str]:
    if left is None or right is None or b"\0" in left[:8192] or b"\0" in right[:8192]:
        return []
    try:
        text_a = left.decode().splitlines()
        text_b = right.decode().splitlines()
    except UnicodeDecodeError:
        return []
    diff = list(difflib.unified_diff(text_a, text_b, f"a/{name}", f"b/{name}", n=1, lineterm=""))
    shown = ["        " + line[:200] for line in diff[:limit]]
    if len(diff) > limit:
        shown.append(f"        ... {len(diff) - limit} more diff lines")
    return shown


def compare_layer(layout_a: Layout, layout_b: Layout, descriptor_a: dict, descriptor_b: dict, files: int, diff_lines: int) -> list[str]:
    order_a, entries_a = layer_entries(layout_a, descriptor_a)
    order_b, entries_b = layer_entries(layout_b, descriptor_b)
    lines: list[str] = []
    kinds: dict[str, int] = {}
    changed: list[str] = []
    count = 0
    for name in order_a:
        if name not in entries_b:
            kinds["only in A"] = kinds.get("only in A", 0) + 1
            count += 1
            changed.append(f"      only in A: {name}")
            continue
        fields_a = entries_a[name].fields()
        fields_b = entries_b[name].fields()
        differing = [key for key in fields_a if fields_a[key] != fields_b[key]]
        if not differing:
            continue
        count += 1
        for key in differing:
            kinds[key] = kinds.get(key, 0) + 1
        detail = ", ".join(f"{key} {fields_a[key]!r} != {fields_b[key]!r}" for key in differing)
        changed.append(f"      {name}: {detail}")
        if "content" in differing:
            changed.extend(text_diff(name, entries_a[name].text, entries_b[name].text, diff_lines))
    for name in order_b:
        if name not in entries_a:
            kinds["only in B"] = kinds.get("only in B", 0) + 1
            count += 1
            changed.append(f"      only in B: {name}")
    if not changed and order_a != order_b:
        common = [name for name in order_a if name in entries_b]
        first = next((position for position, (a, b) in enumerate(zip(common, order_b)) if a != b), None)
        lines.append(f"    same files and metadata, different entry order (first at entry {first})")
        return lines
    if not changed:
        lines.append("    same tar entries; the difference is in the tar or compression framing")
        return lines
    summary = ", ".join(f"{key}: {count}" for key, count in sorted(kinds.items(), key=lambda item: -item[1]))
    lines.append(f"    {count} differing entries ({summary}); first {files}:")
    shown = 0
    for line in changed:
        if not line.startswith("        "):
            shown += 1
            if shown > files:
                break
        lines.append(line)
    return lines


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("image_a", help="OCI layout directory or archive, optionally @sha256:<manifest>")
    parser.add_argument("image_b", help="OCI layout directory or archive, optionally @sha256:<manifest>")
    parser.add_argument("--layers", type=int, default=8, help="differing layers to inspect file by file")
    parser.add_argument("--files", type=int, default=25, help="differing files to print per layer")
    parser.add_argument("--diff-lines", type=int, default=12, help="unified diff lines per differing text file")
    args = parser.parse_args()

    with contextlib.ExitStack() as stack:
        layout_a = stack.enter_context(contextlib.closing(Layout(args.image_a)))
        layout_b = stack.enter_context(contextlib.closing(Layout(args.image_b)))
        return compare(args, layout_a, layout_b)


def compare(args: argparse.Namespace, layout_a: Layout, layout_b: Layout) -> int:
    digest_a, raw_a = layout_a.manifest()
    digest_b, raw_b = layout_b.manifest()
    print(f"A: {args.image_a} manifest {digest_a}")
    print(f"B: {args.image_b} manifest {digest_b}")
    if raw_a == raw_b:
        print("manifests are identical")
        return 0
    manifest_a = json.loads(raw_a)
    manifest_b = json.loads(raw_b)

    config_a = json.loads(layout_a.read_blob(manifest_a["config"]["digest"]))
    config_b = json.loads(layout_b.read_blob(manifest_b["config"]["digest"]))
    config_lines = compare_configs(config_a, config_b)
    print(f"config {short(manifest_a['config']['digest'])} vs {short(manifest_b['config']['digest'])}:")
    print("\n".join(config_lines) if config_lines else "  identical apart from rootfs.diff_ids")
    for key in sorted(set(manifest_a) | set(manifest_b)):
        if key not in ("config", "layers") and manifest_a.get(key) != manifest_b.get(key):
            print(f"manifest.{key}: {json.dumps(manifest_a.get(key))[:300]} != {json.dumps(manifest_b.get(key))[:300]}")

    layers_a = manifest_a.get("layers", [])
    layers_b = manifest_b.get("layers", [])
    history = layer_history(config_a)
    diff_ids_a = config_a.get("rootfs", {}).get("diff_ids", [])
    diff_ids_b = config_b.get("rootfs", {}).get("diff_ids", [])
    differing: list[int] = []
    print(f"layers: {len(layers_a)} vs {len(layers_b)}")
    for position in range(max(len(layers_a), len(layers_b))):
        left = layers_a[position] if position < len(layers_a) else None
        right = layers_b[position] if position < len(layers_b) else None
        same = left is not None and right is not None and left["digest"] == right["digest"]
        if not same:
            differing.append(position)
        uncompressed_same = (
            position < len(diff_ids_a) and position < len(diff_ids_b) and diff_ids_a[position] == diff_ids_b[position]
        )
        status = "same" if same else ("DIFF (compression only)" if uncompressed_same else "DIFF")
        step = history[position][:110] if position < len(history) else ""
        print(
            f"  {position:3d} {status:<23} {short(left['digest']) if left else '-':12} "
            f"{short(right['digest']) if right else '-':12} {left['size'] if left else 0:>11} {step}"
        )
    print(f"{len(differing)} of {max(len(layers_a), len(layers_b))} layers differ")

    for position in differing[: args.layers]:
        if position >= len(layers_a) or position >= len(layers_b):
            continue
        step = history[position][:160] if position < len(history) else ""
        print(f"\nlayer {position}: {step}")
        try:
            for line in compare_layer(layout_a, layout_b, layers_a[position], layers_b[position], args.files, args.diff_lines):
                print(line)
        except (OSError, ValueError, tarfile.TarError, EOFError, ImportError) as error:
            print(f"    cannot compare: {error}")
        sys.stdout.flush()
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, tarfile.TarError) as error:
        sys.exit(f"oci-image-diff: {error}")
