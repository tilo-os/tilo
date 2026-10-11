#!/usr/bin/env python3
"""What an OS image costs to pull and to hold, and where the bytes go.

The OS image is what every install downloads and every update pulls changed
layers of. "Small footprint" is marketed, so its size is a KPI ("Performance
and cost KPIs", Linear af384e878293; TIL-609) with a budget in
tools/perf/kpi-budgets.toml, recorded nightly by Perf / scheduled (footprint
group). This script measures one image without a container engine: it
streams every layer from the registry (or reads an OCI layout), checks each
blob against its digest, and replays the layers with their whiteouts.

Totals, each in bytes:

- `compressed`: the layer blobs a full pull downloads (manifest sizes).
- `payload`: regular-file bytes the layers carry, every layer counted.
- `content`: regular-file bytes of the merged root that install deploys,
  each file once however many hard links name it.
- `unique_content`: `content` with identical files counted once, which is
  what composefs stores after install.
- `undeployed`: files only `/sysroot` names. The base image carries its own
  ostree repository there, hard-linked to its `/usr`; bootc does not deploy
  it, so removing a base package from `/usr` leaves its bytes here: pulled,
  never installed.
- `redundant_payload`: payload beyond one copy of each distinct file: the
  same bytes shipped twice in one image (in two layers or at two paths), or
  a file a later layer replaced. composefs dedupes it on disk; every pull
  still downloads it.

Breakdowns: per layer (with the instruction that made it), the merged root by
directory, the largest files, the largest installed packages (read from the
image's own rpmdb, `rpm -qa --qf '%{SIZE} %{NAME}'`), and the largest groups
of redundant payload. `--why PACKAGE` prints what requires a package, up to
the packages nothing requires (`rpm -q --whatrequires`, transitively).

    python3 tools/perf/image_size.py --image registry.tilo.so/tilo/tilo-ci@sha256:... \\
        --json report.json --markdown report.md
    python3 tools/perf/image_size.py --dev-base registry.tilo.so/tilo/tilo-vm:main-ci --arch x86_64 \\
        --records image.jsonl --runner none
    python3 tools/perf/image_size.py --oci-layout DIR --why python3-botocore

`--dev-base` measures the production-layout base (`tilo-ci`) a dev image
names in its `org.opencontainers.image.base.digest` label: tilo-vm:main-ci
moves forward with main, so the nightly measures main's newest image. The
records carry the image's own source revision as their commit.

`--update-from OLD` adds what an update from OLD to the image downloads: the
compressed layers the image has and OLD does not (bootc fetches a layer only
when its digest is new; TIL-747). `--update-only` reads just the two
manifests, for comparing many pairs of builds quickly:

    python3 tools/perf/image_size.py --image registry.tilo.so/tilo/tilo-ci:main-6503ea1-x86_64 \\
        --update-from registry.tilo.so/tilo/tilo-ci:main-e88a45c-x86_64 --update-only

zstd layers need Python 3.14's `compression.zstd` or the `zstd` command. A
layer that cannot be read fails the run: a size that was not observed is not
a pass.
"""

from __future__ import annotations

import argparse
import collections
import dataclasses
import gzip
import hashlib
import http.client
import io
import json
import os
import re
import shutil
import sqlite3
import struct
import subprocess
import sys
import tarfile
import tempfile
import threading
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import IO, Any, Callable, Iterator

sys.path.insert(0, str(Path(__file__).resolve().parent))

import record  # noqa: E402

SCHEMA = 1
ARCH = {"x86_64": "amd64", "aarch64": "arm64"}
MANIFEST_TYPES = ", ".join((
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.docker.distribution.manifest.v2+json",
))
SHA256 = re.compile(r"sha256:[0-9a-f]{64}\Z")
CHUNK = 1 << 20
RPMDB = "rpmdb.sqlite"
RPMDBS = ("usr/lib/sysimage/rpm/rpmdb.sqlite", "usr/share/rpm/rpmdb.sqlite", "var/lib/rpm/rpmdb.sqlite")
TOP = 30


# ---------------------------------------------------------------- sources


class Registry:
    """Anonymous (or bearer-token) reads from an OCI distribution registry."""

    def __init__(self, host: str, repository: str, scheme: str = "https") -> None:
        self.host, self.repository, self.scheme = host, repository, scheme
        self.token: str | None = None

    def open(self, path: str, accept: str | None = None, start: int = 0) -> IO[bytes]:
        url = f"{self.scheme}://{self.host}/v2/{self.repository}/{path}"
        for attempt in range(2):
            headers = {"User-Agent": "tilo-image-size/1"}
            if accept:
                headers["Accept"] = accept
            if start:
                headers["Range"] = f"bytes={start}-"
            request = urllib.request.Request(url, headers=headers)
            if self.token:
                # Not carried across the registry's redirect to its blob store,
                # which refuses a second authorization (HTTP 400).
                request.add_unredirected_header("Authorization", f"Bearer {self.token}")
            try:
                reply = urllib.request.urlopen(request, timeout=120)
                if start and reply.status != 206:
                    reply.close()
                    raise ValueError(f"{url}: the registry ignored a resumed range request")
                return reply
            except urllib.error.HTTPError as error:
                challenge = error.headers.get("WWW-Authenticate", "")
                if error.code != 401 or attempt or not challenge.startswith("Bearer "):
                    raise ValueError(f"{url}: HTTP {error.code}") from error
                self.token = self.fetch_token(challenge)
        raise AssertionError("unreachable")

    def fetch_token(self, challenge: str) -> str:
        fields = dict(re.findall(r'(\w+)="([^"]*)"', challenge))
        query = {key: fields[key] for key in ("service", "scope") if key in fields}
        query.setdefault("scope", f"repository:{self.repository}:pull")
        with urllib.request.urlopen(f"{fields['realm']}?{urllib.parse.urlencode(query)}", timeout=60) as reply:
            body = json.load(reply)
        token = body.get("token") or body.get("access_token")
        if not token:
            raise ValueError(f"{self.host}: the token endpoint returned no token")
        return token

    def json(self, path: str, accept: str | None = None) -> tuple[dict[str, Any], str]:
        with self.open(path, accept) as reply:
            data = reply.read()
        return json.loads(data), "sha256:" + hashlib.sha256(data).hexdigest()

    def manifest(self, reference: str) -> tuple[dict[str, Any], str]:
        return self.json(f"manifests/{reference}", MANIFEST_TYPES)

    def blob(self, digest: str) -> IO[bytes]:
        return io.BufferedReader(Resuming(lambda start: self.open(f"blobs/{digest}", start=start)), CHUNK)

    def blob_json(self, digest: str) -> dict[str, Any]:
        data, actual = self.json(f"blobs/{digest}")
        if actual != digest:
            raise ValueError(f"blob {digest} fails its digest check")
        return data


class Resuming(io.RawIOBase):
    """A download that reopens at its offset when the connection drops.

    Gigabyte layers over a shared runner's network time out now and then; the
    digest check over the whole blob still decides whether the bytes are
    right."""

    RETRIES = 4

    def __init__(self, opener: Callable[[int], IO[bytes]]) -> None:
        self.opener, self.offset, self.failures = opener, 0, 0
        self.stream: IO[bytes] = opener(0)

    def readable(self) -> bool:
        return True

    def readinto(self, buffer: Any) -> int:
        while True:
            try:
                data = self.stream.read(len(buffer))
            except (OSError, http.client.HTTPException) as error:
                self.failures += 1
                if self.failures > self.RETRIES:
                    raise ValueError(f"download failed {self.failures} times at byte {self.offset}: {error}") from error
                self.stream.close()
                self.stream = self.opener(self.offset)
                continue
            size = len(data)
            buffer[:size] = data
            self.offset += size
            return size

    def close(self) -> None:
        self.stream.close()
        super().close()


class Layout:
    """An OCI image layout directory (`oci-layout`, `index.json`, `blobs/`)."""

    def __init__(self, root: Path) -> None:
        self.root = root

    def path(self, digest: str) -> Path:
        if not SHA256.fullmatch(digest):
            raise ValueError(f"invalid digest {digest}")
        return self.root / "blobs" / "sha256" / digest[7:]

    def blob(self, digest: str) -> IO[bytes]:
        return self.path(digest).open("rb")

    def blob_json(self, digest: str) -> dict[str, Any]:
        data = self.path(digest).read_bytes()
        if "sha256:" + hashlib.sha256(data).hexdigest() != digest:
            raise ValueError(f"blob {digest} fails its digest check")
        return json.loads(data)

    def index(self) -> dict[str, Any]:
        return json.loads((self.root / "index.json").read_text(encoding="utf-8"))


def parse_reference(text: str) -> tuple[str, str, str]:
    """`host/repo[:tag|@digest]` as (host, repository, tag or digest)."""
    name, at, digest = text.partition("@")
    if not at:
        slash = name.rfind("/")
        colon = name.rfind(":")
        name, digest = (name[:colon], name[colon + 1:]) if colon > slash else (name, "latest")
    host, slash, repository = name.partition("/")
    if not slash or "." not in host:
        raise ValueError(f"{text}: name the registry host, e.g. registry.tilo.so/tilo/tilo-ci:TAG")
    return host, repository, digest


def select_platform(index: dict[str, Any], arch: str | None) -> str:
    manifests = [item for item in index.get("manifests", [])
                 if item.get("platform", {}).get("os", "linux") == "linux"]
    if arch:
        manifests = [item for item in manifests if item.get("platform", {}).get("architecture") == ARCH[arch]]
    if len(manifests) != 1:
        raise ValueError(f"the index holds {len(manifests)} linux manifests for {arch or 'any architecture'}; "
                         "pass --arch")
    return manifests[0]["digest"]


@dataclasses.dataclass
class Image:
    reference: str
    manifest_digest: str
    manifest: dict[str, Any]
    config: dict[str, Any]
    blob: Callable[[str], IO[bytes]]

    @property
    def labels(self) -> dict[str, str]:
        return self.config.get("config", {}).get("Labels") or {}

    @property
    def arch(self) -> str:
        names = {value: key for key, value in ARCH.items()}
        return names.get(self.config.get("architecture", ""), self.config.get("architecture", "unknown"))


def is_index(document: dict[str, Any]) -> bool:
    return "manifests" in document and "layers" not in document


def registry_image(reference: str, arch: str | None) -> Image:
    host, repository, ref = parse_reference(reference)
    registry = Registry(host, repository)
    document, digest = registry.manifest(ref)
    if is_index(document):
        digest = select_platform(document, arch)
        document, actual = registry.manifest(digest)
        if actual != digest:
            raise ValueError(f"manifest {digest} fails its digest check")
    elif SHA256.fullmatch(ref) and ref != digest:
        raise ValueError(f"manifest {ref} fails its digest check")
    config = registry.blob_json(document["config"]["digest"])
    return Image(f"{host}/{repository}@{digest}", digest, document, config, registry.blob)


def dev_base(reference: str, arch: str) -> str:
    """The production-layout base a dev image (tilo-vm) was built on."""
    dev = registry_image(reference, arch)
    base = dev.labels.get("org.opencontainers.image.base.name", "")
    digest = dev.labels.get("org.opencontainers.image.base.digest", "")
    if not SHA256.fullmatch(digest) or not base.endswith("@" + digest):
        raise ValueError(f"{reference}: no org.opencontainers.image.base.name/.digest label naming its base")
    return base


def layout_image(root: Path, arch: str | None) -> Image:
    layout = Layout(root)
    index = layout.index()
    digest = select_platform(index, arch) if len(index.get("manifests", [])) != 1 else index["manifests"][0]["digest"]
    document = layout.blob_json(digest)
    if is_index(document):
        digest = select_platform(document, arch)
        document = layout.blob_json(digest)
    config = layout.blob_json(document["config"]["digest"])
    return Image(f"oci:{root}@{digest}", digest, document, config, layout.blob)


# ---------------------------------------------------------------- reading


class Verified(io.RawIOBase):
    """A blob stream that counts and hashes what it hands out."""

    def __init__(self, stream: IO[bytes]) -> None:
        self.stream, self.hash, self.count = stream, hashlib.sha256(), 0

    def readable(self) -> bool:
        return True

    def readinto(self, buffer: Any) -> int:
        data = self.stream.read(len(buffer))
        size = len(data)
        buffer[:size] = data
        self.hash.update(data)
        self.count += size
        return size

    def drain(self) -> None:
        while self.readinto(bytearray(CHUNK)):
            pass

    @property
    def digest(self) -> str:
        return "sha256:" + self.hash.hexdigest()


class ZstdPipe(io.RawIOBase):
    """`zstd -dc` over a stream, for a Python without compression.zstd."""

    def __init__(self, stream: IO[bytes]) -> None:
        if shutil.which("zstd") is None:
            raise ValueError("a zstd layer needs Python 3.14's compression.zstd or the zstd command")
        self.process = subprocess.Popen(["zstd", "-dc", "-q"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        self.error: BaseException | None = None
        self.feeder = threading.Thread(target=self.feed, args=(stream,), daemon=True)
        self.feeder.start()

    def feed(self, stream: IO[bytes]) -> None:
        assert self.process.stdin is not None
        try:
            while chunk := stream.read(CHUNK):
                self.process.stdin.write(chunk)
        except BaseException as error:  # handed to the reader, which raises it
            self.error = error
        finally:
            self.process.stdin.close()

    def readable(self) -> bool:
        return True

    def readinto(self, buffer: Any) -> int:
        assert self.process.stdout is not None
        size = self.process.stdout.readinto(buffer)
        if not size:
            self.feeder.join()
            if self.process.wait() != 0 or self.error:
                raise ValueError(f"zstd failed to decompress a layer: {self.error or self.process.returncode}")
        return size


def decompressed(media_type: str, stream: IO[bytes]) -> IO[bytes]:
    if media_type.endswith("+gzip") or media_type.endswith(".tar.gzip"):
        return gzip.GzipFile(fileobj=stream, mode="rb")
    if media_type.endswith("+zstd"):
        try:
            from compression import zstd  # type: ignore[import-not-found]
        except ImportError:
            return io.BufferedReader(ZstdPipe(stream), CHUNK)
        return zstd.ZstdFile(stream, mode="rb")
    if media_type.endswith(".tar") or media_type.endswith("tar.v1"):
        return stream
    raise ValueError(f"unsupported layer media type {media_type}")


@dataclasses.dataclass
class Entry:
    size: int
    sha: str
    layer: int


@dataclasses.dataclass
class Layer:
    digest: str
    media_type: str
    compressed: int
    created_by: str
    payload: int = 0
    files: int = 0


def normal(name: str) -> str:
    if name.startswith("./"):
        name = name[2:]
    return name.lstrip("/").rstrip("/")


def instructions(config: dict[str, Any]) -> list[str]:
    """The history entries that made a layer, in layer order."""
    made = []
    for item in config.get("history", []):
        if item.get("empty_layer"):
            continue
        text = str(item.get("created_by", ""))
        # BuildKit prefixes RUN with `|N ARG=value ...`: build arguments, not the command.
        match = re.match(r"(RUN )\|(\d+) ", text)
        if match:
            text = match.group(1) + text[match.end():].split(" ", int(match.group(2)))[-1]
        text = text.replace(" # buildkit", "")
        made.append(re.sub(r"\s+", " ", text).strip()[:160])
    return made


class Root:
    """The merged root while layers replay, with every layer's payload."""

    def __init__(self) -> None:
        self.files: dict[str, Entry] = {}
        # Hard links by path, to the file they share an inode with.
        self.links: dict[str, Entry] = {}
        # content hash -> (size, [(layer, path)]) over every layer's payload
        self.payload: dict[str, tuple[int, list[tuple[int, str]]]] = {}
        # The bytes of every rpmdb.sqlite the layers carry, by path; links alias them.
        self.kept: dict[str, bytes] = {}

    def replace(self, path: str) -> None:
        """A layer's entry at `path` replaces the lower one."""
        self.files.pop(path, None)
        self.links.pop(path, None)

    def remove(self, path: str, below_layer: int | None = None) -> None:
        """A whiteout of `path` and everything below it (opaque: from lower layers only)."""
        prefix = path + "/" if path else ""
        for name in [name for name in self.files if name == path or name.startswith(prefix)]:
            if below_layer is None or self.files[name].layer < below_layer:
                del self.files[name]
        if below_layer is None:
            for name in [name for name in self.links if name == path or name.startswith(prefix)]:
                del self.links[name]

    def apply(self, index: int, layer: Layer, archive: tarfile.TarFile) -> None:
        for member in archive:
            path = normal(member.name)
            directory, _, base = path.rpartition("/")
            if base == ".wh..wh..opq":
                self.remove(directory, below_layer=index)
                continue
            if base.startswith(".wh."):
                self.remove(f"{directory}/{base[4:]}" if directory else base[4:])
                continue
            if not member.isdir():
                self.replace(path)
            if member.islnk():
                target = normal(member.linkname)
                entry = self.files.get(target) or self.links.get(target)
                if entry is not None:
                    self.links[path] = entry
                if target in self.kept:
                    self.kept[path] = self.kept[target]
            if not member.isreg():
                continue
            stream = archive.extractfile(member)
            assert stream is not None
            digest = hashlib.sha256()
            keep = base == RPMDB
            kept = bytearray()
            while chunk := stream.read(CHUNK):
                digest.update(chunk)
                if keep:
                    kept += chunk
            sha = digest.hexdigest()
            self.files[path] = Entry(member.size, sha, index)
            layer.payload += member.size
            layer.files += 1
            size, where = self.payload.setdefault(sha, (member.size, []))
            where.append((index, path))
            if keep:
                self.kept[path] = bytes(kept)

    @property
    def rpmdb(self) -> bytes | None:
        """The merged root's rpm database (rpm's own path first)."""
        present = [path for path in self.kept if path in self.files or path in self.links]
        for path in sorted(present, key=lambda name: (name not in RPMDBS, name)):
            return self.kept[path]
        return None


def replay(image: Image, progress: Callable[[str], None] | None = None) -> tuple[list[Layer], Root]:
    made = instructions(image.config)
    layers = image.manifest.get("layers", [])
    diff_ids = image.config.get("rootfs", {}).get("diff_ids", [])
    if len(layers) != len(diff_ids):
        raise ValueError("the manifest's layers and the config's diff IDs disagree")
    root = Root()
    result = []
    for index, item in enumerate(layers):
        digest = item["digest"]
        if not SHA256.fullmatch(digest):
            raise ValueError(f"invalid layer digest {digest}")
        layer = Layer(digest, item["mediaType"], int(item["size"]), made[index] if index < len(made) else "")
        with image.blob(digest) as raw:
            verified = Verified(raw)
            stream = decompressed(layer.media_type, io.BufferedReader(verified, CHUNK))
            with tarfile.open(fileobj=stream, mode="r|") as archive:
                root.apply(index, layer, archive)
            verified.drain()
        if verified.digest != digest or verified.count != layer.compressed:
            raise ValueError(f"layer {digest} fails its digest check ({verified.count} bytes read)")
        result.append(layer)
        if progress:
            progress(f"layer {index + 1}/{len(layers)} {digest[7:19]} {layer.compressed:,} B "
                     f"-> {layer.payload:,} B {layer.created_by[:60]}")
    return result, root


# ---------------------------------------------------------------- packages

RPM_TAGS = {1000: "name", 1001: "version", 1002: "release", 1009: "size", 5009: "longsize",
            1047: "provides", 1049: "requires", 5046: "recommends", 5052: "supplements",
            1116: "dirindexes", 1117: "basenames", 1118: "dirnames"}


def rpm_header(blob: bytes) -> dict[str, Any]:
    """The tags of one rpmdb.sqlite Packages blob (a header without its magic)."""
    count, store = struct.unpack(">II", blob[:8])
    data = blob[8 + 16 * count:]
    if len(data) < store:
        raise ValueError("truncated rpm header")
    tags: dict[str, Any] = {}
    for position in range(count):
        tag, kind, offset, items = struct.unpack(">iiii", blob[8 + 16 * position:24 + 16 * position])
        name = RPM_TAGS.get(tag)
        if name is None:
            continue
        if kind == 4:
            tags[name] = list(struct.unpack(f">{items}I", data[offset:offset + 4 * items]))
        elif kind == 5:
            tags[name] = list(struct.unpack(f">{items}Q", data[offset:offset + 8 * items]))
        elif kind in (6, 8, 9):
            strings = data[offset:].split(b"\0", items)[:items]
            tags[name] = [value.decode("utf-8", "replace") for value in strings]
    return tags


@dataclasses.dataclass
class Package:
    name: str
    nevr: str
    size: int
    provides: set[str]
    requires: set[str]
    recommends: set[str]
    supplements: set[str]
    files: set[str]


def packages(rpmdb: bytes) -> list[Package]:
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "rpmdb.sqlite"
        path.write_bytes(rpmdb)
        connection = sqlite3.connect(f"file:{path}?mode=ro&immutable=1", uri=True)
        try:
            rows = connection.execute("SELECT blob FROM Packages").fetchall()
        finally:
            connection.close()
    result = []
    for (blob,) in rows:
        tags = rpm_header(bytes(blob))
        name = tags.get("name", ["?"])[0]
        if name == "gpg-pubkey":
            continue
        size = (tags.get("longsize") or tags.get("size") or [0])[0]
        dirs, bases, indexes = tags.get("dirnames", []), tags.get("basenames", []), tags.get("dirindexes", [])
        files = {dirs[indexes[i]] + base for i, base in enumerate(bases) if i < len(indexes)}
        result.append(Package(
            name, f"{name}-{tags.get('version', ['?'])[0]}-{tags.get('release', ['?'])[0]}", int(size),
            set(tags.get("provides", [])), set(tags.get("requires", [])),
            set(tags.get("recommends", [])), set(tags.get("supplements", [])), files))
    return result


RICH_WORDS = frozenset({"with", "without", "if", "else", "unless", "and", "or"})
COMPARISONS = frozenset({"<", "<=", "=", ">=", ">", "=="})


def capabilities(requirement: str) -> set[str]:
    """The capability names a requirement may be met by, rich dependencies included.

    `(python3.14dist(botocore) < 2~a0 with python3.14dist(botocore) >= 1.37.4)`
    names `python3.14dist(botocore)`; a plain `name` names itself."""
    if not requirement.startswith("("):
        return {requirement}
    names, previous = set(), ""
    for token in re.findall(r"[^\s()]+(?:\([^\s()]*\))?|[()]", requirement):
        if token not in "()" and token not in RICH_WORDS and token not in COMPARISONS \
                and previous not in COMPARISONS:
            names.add(token)
        previous = token
    return names


def requirers(target: Package, everything: list[Package]) -> list[tuple[str, str, str]]:
    """(package, relation, requirement) for every package that pulls `target` in."""
    offered = target.provides | target.files | {target.name}
    found = []
    for package in everything:
        if package.name == target.name:
            continue
        for relation, wanted in (("requires", package.requires), ("recommends", package.recommends)):
            for requirement in sorted(wanted):
                if capabilities(requirement) & offered:
                    found.append((package.name, relation, requirement))
                    break
    return found


def why(name: str, everything: list[Package], depth: int = 8) -> list[str]:
    """Reverse-dependency chains from `name` up to packages nothing requires."""
    by_name = {package.name: package for package in everything}
    if name not in by_name:
        return [f"{name} is not installed"]
    lines = []
    seen: set[str] = set()

    def walk(current: str, level: int) -> None:
        pulls = requirers(by_name[current], everything)
        if not pulls:
            lines.append("  " * level + f"{current}: nothing requires it (installed by name)")
            return
        for package, relation, capability in pulls:
            mark = " (seen above)" if package in seen else ""
            lines.append("  " * level + f"{current} <- {package} {relation} {capability}{mark}")
            if package not in seen and level < depth:
                seen.add(package)
                walk(package, level + 1)

    seen.add(name)
    walk(name, 0)
    return lines


# ---------------------------------------------------------------- report


def directory_totals(files: dict[str, Entry], depth: int) -> dict[str, int]:
    totals: collections.Counter[str] = collections.Counter()
    for path, entry in files.items():
        parts = path.split("/")
        if len(parts) > depth:
            totals["/" + "/".join(parts[:depth])] += entry.size
    return dict(totals)


# The image's own ostree repository: the base's objects, hard-linked to its
# /usr. bootc does not deploy /sysroot from an image (the receipt in
# docs/benchmarks/os-runtime-current-main-2026-10-06.md: deleting it leaves the
# composefs root digest unchanged), so a file only it names is pulled but never
# installed.
UNDEPLOYED = "sysroot/"


def inodes(root: Root) -> list[tuple[Entry, list[str]]]:
    """Every file of the merged root once, with all its names, deployed names first."""
    groups: dict[int, tuple[Entry, list[str]]] = {}
    for path, entry in [*root.files.items(), *root.links.items()]:
        groups.setdefault(id(entry), (entry, []))[1].append(path)
    for _, names in groups.values():
        names.sort(key=lambda name: (name.startswith(UNDEPLOYED), name))
    return list(groups.values())


def deployed(names: list[str]) -> bool:
    return not names[0].startswith(UNDEPLOYED)


def report(image: Image, layers: list[Layer], root: Root) -> dict[str, Any]:
    groups = inodes(root)
    placed = {names[0]: entry for entry, names in groups}
    unique: dict[str, int] = {}
    for entry, names in groups:
        if deployed(names):
            unique[entry.sha] = entry.size
    payload = sum(layer.payload for layer in layers)
    distinct = sum(size for size, _ in root.payload.values())
    redundant = []
    for sha, (size, where) in root.payload.items():
        if len(where) > 1 and size:
            redundant.append({"sha256": sha, "size": size, "copies": len(where),
                              "redundant": size * (len(where) - 1),
                              "paths": sorted({f"/{path}" for _, path in where})[:6],
                              "layers": sorted({index for index, _ in where})})
    redundant.sort(key=lambda item: -item["redundant"])
    # Each extra copy is charged to where it lives: the first copy, in layer
    # order, is the one the image needs.
    redundant_dirs: collections.Counter[str] = collections.Counter()
    for size, where in root.payload.values():
        for _, path in where[1:]:
            redundant_dirs["/" + "/".join(path.split("/")[:4])] += size
    rpmdb = root.rpmdb
    installed = packages(rpmdb) if rpmdb else []
    data = {
        "schema": SCHEMA,
        "image": image.reference,
        "manifest_digest": image.manifest_digest,
        "config_digest": image.manifest["config"]["digest"],
        "architecture": image.arch,
        "revision": image.labels.get("org.opencontainers.image.revision", ""),
        "labels": {key: value for key, value in image.labels.items() if key.startswith("org.tilo.")},
        "totals": {
            "layers": len(layers),
            "compressed": sum(layer.compressed for layer in layers),
            "payload": payload,
            "content": sum(entry.size for entry, names in groups if deployed(names)),
            "unique_content": sum(unique.values()),
            "undeployed": sum(entry.size for entry, names in groups if not deployed(names)),
            "redundant_payload": payload - distinct,
            "files": sum(deployed(names) for _, names in groups),
            "hard_links": len(root.links),
            "packages": len(installed),
        },
        "layers": [dataclasses.asdict(layer) for layer in layers],
        "directories": {
            "depth2": sorted(directory_totals(placed, 2).items(), key=lambda kv: -kv[1])[:TOP],
            "depth3": sorted(directory_totals(placed, 3).items(), key=lambda kv: -kv[1])[:TOP],
        },
        "files": sorted(([f"/{path}", entry.size] for path, entry in placed.items()),
                        key=lambda item: -item[1])[:TOP],
        "packages": sorted(([package.name, package.size] for package in installed),
                           key=lambda item: -item[1])[:TOP],
        "redundant": redundant[:3 * TOP],
        "redundant_by_directory": redundant_dirs.most_common(TOP),
    }
    if not rpmdb:
        data["packages_note"] = "no rpmdb.sqlite in the image"
    return data


def update(image: Image, old: Image) -> dict[str, Any]:
    """What a device that has `old` downloads to move to `image`: the layers whose digests are new."""
    have = {item["digest"] for item in old.manifest.get("layers", [])}
    layers = image.manifest.get("layers", [])
    new = [item for item in layers if item["digest"] not in have]
    compressed = sum(int(item["size"]) for item in layers)
    downloaded = sum(int(item["size"]) for item in new)
    return {"from": old.reference, "from_revision": old.labels.get("org.opencontainers.image.revision", ""),
            "layers": len(layers), "new_layers": len(new), "bytes": downloaded, "compressed": compressed,
            "share": round(downloaded / compressed, 4) if compressed else 0.0}


def update_markdown(data: dict[str, Any]) -> list[str]:
    item = data["update"]
    return ["", "## Update", "",
            f"From `{item['from']}` (revision `{item['from_revision']}`): {item['new_layers']} of "
            f"{item['layers']} layers are new, **{item['bytes']:,} B** ({mb(item['bytes'])}, "
            f"{100 * item['share']:.1f} % of {item['compressed']:,} B)."]


def mb(value: int) -> str:
    return f"{value / 1e6:,.1f} MB"


def markdown(data: dict[str, Any], why_lines: dict[str, list[str]]) -> str:
    totals = data["totals"]
    lines = [f"# Image size: `{data['image']}`", "",
             f"Revision `{data['revision']}`, {data['architecture']}, {totals['layers']} layers, "
             f"{totals['files']:,} files, {totals['packages']} packages.", "",
             "| measure | bytes | |", "|---|---:|---:|"]
    for name in ("compressed", "payload", "content", "unique_content", "undeployed", "redundant_payload"):
        lines.append(f"| {name} | {totals[name]:,} | {mb(totals[name])} |")
    def table(title: str, rows: list[Any], header: str = "| path | bytes |") -> None:
        lines.extend(["", f"## {title}", "", header, "|---|---:|"])
        lines.extend(f"| `{name}` | {size:,} |" for name, size in rows)
    table("Largest layers (compressed)", [
        (f"{layer['digest'][7:19]} {layer['created_by'][:90]}", layer["compressed"])
        for layer in sorted(data["layers"], key=lambda item: -item["compressed"])[:TOP]], "| layer | bytes |")
    table("Merged root by directory", data["directories"]["depth3"])
    table("Largest packages", data["packages"], "| package | bytes |")
    table("Largest files", data["files"])
    table("Redundant payload by directory", data["redundant_by_directory"])
    for name, chain in why_lines.items():
        lines.extend(["", f"## Why `{name}` is installed", "", "```", *chain, "```"])
    return "\n".join(lines) + "\n"


def records(data: dict[str, Any], runner: str, hardware: str, started_at: str) -> list[record.Record]:
    totals, arch = data["totals"], data["architecture"]
    lto = data["labels"].get("org.tilo.rust-lto", "")
    env = {"commit": data["revision"] or data["manifest_digest"],
           # LTO changes every Tilo binary's size, so it is part of the comparison key.
           "profile": "release" if lto in ("thin", "fat") else "dev",
           "renderer": "none", "virtualization": "none", "runner": runner,
           "hardware": hardware, "started_at": started_at,
           "meta": {"image": data["image"], "rust_lto": lto or "unknown",
                    "layers": str(totals["layers"]), "files": str(totals["files"])}}
    return [record.from_samples(f"footprint.image.{name}.{arch}", "bytes", [totals[key]], **env)
            for name, key in (("compressed_bytes", "compressed"), ("unique_content_bytes", "unique_content"),
                              ("redundant_payload_bytes", "redundant_payload"))]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--image", help="registry reference, host/repository[:tag|@digest]")
    source.add_argument("--dev-base", help="a dev image whose base label names the image to measure")
    source.add_argument("--oci-layout", type=Path, help="an OCI image layout directory")
    parser.add_argument("--arch", choices=sorted(ARCH), help="the platform to read from an index")
    parser.add_argument("--json", type=Path, help="write the full report here")
    parser.add_argument("--markdown", type=Path, help="write a Markdown summary here")
    parser.add_argument("--records", type=Path, help="append footprint.image.* records here")
    parser.add_argument("--runner", default="none", help="records' runner (none: a byte count)")
    parser.add_argument("--hardware", default="", help="records' hardware description")
    parser.add_argument("--why", action="append", default=[], metavar="PACKAGE",
                        help="print what pulls PACKAGE into the image")
    parser.add_argument("--save-rpmdb", type=Path, help="write the merged root's rpmdb.sqlite here")
    parser.add_argument("--update-from", metavar="OLD",
                        help="also report what updating from OLD downloads (a reference or oci:DIR)")
    parser.add_argument("--update-only", action="store_true",
                        help="with --update-from: read the two manifests only, no layers")
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args(argv)
    progress = None if args.quiet else (lambda line: print(line, file=sys.stderr, flush=True))
    try:
        if args.update_only and not args.update_from:
            raise ValueError("--update-only needs --update-from")
        if args.oci_layout:
            image = layout_image(args.oci_layout, args.arch)
        else:
            reference = args.image
            if args.dev_base:
                if not args.arch:
                    raise ValueError("--dev-base needs --arch")
                reference = dev_base(args.dev_base, args.arch)
            image = registry_image(reference, args.arch)
        if args.arch and image.arch != args.arch:
            raise ValueError(f"{image.reference} is {image.arch}, not {args.arch}")
        moved = None
        if args.update_from:
            old = (layout_image(Path(args.update_from[4:]), args.arch) if args.update_from.startswith("oci:")
                   else registry_image(args.update_from, args.arch))
            if old.arch != image.arch:
                raise ValueError(f"{old.reference} is {old.arch}, {image.reference} is {image.arch}")
            moved = update(image, old)
        if args.update_only:
            assert moved is not None
            data = {"schema": SCHEMA, "image": image.reference, "manifest_digest": image.manifest_digest,
                    "architecture": image.arch,
                    "revision": image.labels.get("org.opencontainers.image.revision", ""), "update": moved}
            if args.json:
                args.json.write_text(json.dumps(data, indent=1) + "\n", encoding="utf-8")
            text = "\n".join([f"# Update to `{image.reference}`", "", *update_markdown(data)[3:]]) + "\n"
            if args.markdown:
                args.markdown.write_text(text, encoding="utf-8")
            if not args.quiet:
                print(text, end="")
            return 0
        layers, root = replay(image, progress)
        data = report(image, layers, root)
        rpmdb = root.rpmdb
        installed = packages(rpmdb) if rpmdb else []
        chains = {name: why(name, installed) for name in args.why}
        data["why"] = chains
        data["rpmdb_paths"] = sorted(root.kept)
        if moved is not None:
            data["update"] = moved
        if args.save_rpmdb and rpmdb:
            args.save_rpmdb.write_bytes(rpmdb)
        if args.json:
            args.json.write_text(json.dumps(data, indent=1) + "\n", encoding="utf-8")
        text = markdown(data, chains)
        if moved is not None:
            text += "\n".join(update_markdown(data)) + "\n"
        if args.markdown:
            args.markdown.write_text(text, encoding="utf-8")
        if args.records:
            hardware = args.hardware or f"OCI layers of {image.reference}"
            with args.records.open("a", encoding="utf-8") as stream:
                for item in records(data, args.runner, hardware, record.utc_now()):
                    stream.write(item.to_json() + "\n")
        if not args.quiet:
            print(text, end="")
        if args.why and not installed:
            raise ValueError("--why needs the image's rpmdb, and the image has none")
    except (ValueError, OSError, sqlite3.Error, tarfile.TarError, KeyError) as error:
        print(f"image_size.py: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
