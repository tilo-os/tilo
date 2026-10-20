#!/usr/bin/env python3
"""Size and load-cost budget for the shared app runtime, `libruntime_dylib.so`.

Every thin app maps this one Rust dylib (crates/runtime/runtime-dylib), so its
size is what a device downloads when an app's runtime is missing, and its
dynamic relocations are work the loader repeats, and memory it dirties, in
every app process before `main`. The KPI holds four numbers per
architecture, all read from the ELF file itself:

- `bytes`: the file;
- `relocation_bytes`: the dynamic relocation tables (`SHT_RELA` and
  `SHT_RELR` sections) the loader reads at every app start;
- `relocated_bytes`: the distinct 4 KiB pages those relocations write, which
  become private, dirty memory in each app process;
- `symbol_relocations`: relocations that need a symbol lookup (everything
  but the relative ones), the loader's slow path.

It also names the `DT_NEEDED` libraries. A media library there would be loaded
by every app, media or not: `[targets] needed_absent` names them, and the
build gate fails when one is in the runtime's `DT_NEEDED`, in the closure of
libraries every app loads with it, or in the closure of a built app that
`apps_without_media` lists. GLib and GStreamer reach the runtime
through `tools/ci/runtime-dylib-lazy-link.py`, which loads them on first use
and records them in the `.tilo.lazy_needed` section; the gate reports those.

Engines a few apps use live in libraries of their own beside the runtime
(crates/runtime/engines/). `measure --engine FILE` reports each one's
numbers, fails when one does not link the runtime, and fails when an app of
`apps_without_media` loads one at exec: an engine is loaded only by its apps,
and an app of that list that uses one is named in `[engine_users]`.

Three gates, as for the per-app tapp budgets (tapp_size_ratchet.py):

- **The build.** `measure FILE --arch ARCH` reads the dylib Release / apps has
  just built (`target/thin-app/libruntime_dylib.so`) and fails when a number
  is over its budget.
- **The published runtime, nightly.** `published ENTRIES_JSON` reads the
  registry index, fetches the newest `so.tilo.runtime` of each architecture,
  measures its dylib and holds it to the same budgets (Perf / scheduled,
  footprint group). `--measurements FILE` writes what it measured, which
  `adapters.py runtime-sizes` turns into `footprint.runtime.*` records.
- **The change.** `check-budget --base REV` (t's `changes` check) fails when a
  budget grows beyond what raise files new in the change add.

`tools/perf/runtime-size-budget/budget.toml` holds the budgets: a measurement
plus `headroom`, rounded up to 64 KiB (counts to 64). Intended growth adds one
`raises/<change>.toml` per change. When the runtime shrinks, both gates print
the lower budget to commit. docs/measurements/runtime-size.md has the steps.
"""

from __future__ import annotations

import argparse
import io
import json
import math
import os
import pathlib
import struct
import sys
import tarfile
import tempfile
import tomllib
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Callable

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE))

import tapp_size_ratchet as tapp  # noqa: E402

DIRECTORY = "tools/perf/runtime-size-budget"
METRICS = ("bytes", "relocation_bytes", "relocated_bytes", "symbol_relocations")
# Byte metrics round to 64 KiB like the tapp budgets; a count rounds to 64.
ROUNDING = {"bytes": 64 * 1024, "relocation_bytes": 64 * 1024, "relocated_bytes": 64 * 1024,
            "symbol_relocations": 64}
SLACK = tapp.SLACK
MIN_REASON = tapp.MIN_REASON
PAGE = 4096
RUNTIME_PACKAGE = "so.tilo.runtime"
DYLIB = "lib/libruntime_dylib.so"
REGISTRY = "https://apps.tilo.so"
TIMEOUT_SECONDS = 120

# ELF constants (64-bit little-endian only: both shipped architectures).
SHT_RELA, SHT_DYNAMIC, SHT_RELR = 4, 6, 19
# The libraries tools/ci/runtime-dylib-lazy-link.py loads on first use.
LAZY_SECTION = ".tilo.lazy_needed"
# Where the loader finds what no RUNPATH or library directory names.
SYSTEM_DIRECTORIES = tuple(pathlib.Path(directory) for directory in (
    "/lib64", "/usr/lib64", "/lib", "/usr/lib", "/lib/x86_64-linux-gnu", "/usr/lib/x86_64-linux-gnu",
    "/lib/aarch64-linux-gnu", "/usr/lib/aarch64-linux-gnu"))
DT_NEEDED = 1
RELATIVE = {62: 8, 183: 1027}  # e_machine -> R_*_RELATIVE: x86_64, aarch64
MACHINES = {62: "x86_64", 183: "aarch64"}


@dataclass
class ElfStats:
    arch: str
    bytes: int
    sections: dict[str, int] = field(default_factory=dict)
    needed: list[str] = field(default_factory=list)
    lazy_needed: list[str] = field(default_factory=list)
    relocations: int = 0
    symbol_relocations: int = 0
    relocation_bytes: int = 0
    relocated_bytes: int = 0

    def metrics(self) -> dict[str, int]:
        return {name: getattr(self, name) for name in METRICS}


def _cstring(table: bytes, offset: int) -> str:
    end = table.index(b"\0", offset)
    return table[offset:end].decode("utf-8", "replace")


def _relr_addresses(data: bytes) -> list[int]:
    """The addresses a packed relative relocation section (DT_RELR) relocates."""
    addresses: list[int] = []
    base = 0
    for (entry,) in struct.iter_unpack("<Q", data):
        if entry & 1 == 0:
            addresses.append(entry)
            base = entry + 8
            continue
        bits = entry >> 1
        index = 0
        while bits:
            if bits & 1:
                addresses.append(base + index * 8)
            bits >>= 1
            index += 1
        base += 63 * 8
    return addresses


def read_elf(path: pathlib.Path) -> ElfStats:
    """Section sizes, DT_NEEDED and dynamic relocations of a 64-bit LE ELF file."""
    data = path.read_bytes()
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        raise ValueError(f"{path}: not a 64-bit little-endian ELF file")
    machine = struct.unpack_from("<H", data, 18)[0]
    if machine not in MACHINES:
        raise ValueError(f"{path}: unsupported ELF machine {machine}")
    shoff = struct.unpack_from("<Q", data, 40)[0]
    shentsize, shnum, shstrndx = struct.unpack_from("<HHH", data, 58)
    headers = [struct.unpack_from("<IIQQQQIIQQ", data, shoff + index * shentsize) for index in range(shnum)]
    if not headers:
        raise ValueError(f"{path}: no section headers (stripped too far to measure)")
    names_header = headers[shstrndx]
    names = data[names_header[4]:names_header[4] + names_header[5]]
    stats = ElfStats(arch=MACHINES[machine], bytes=len(data))
    pages: set[int] = set()
    relative = RELATIVE[machine]
    for name_offset, kind, _flags, _addr, offset, size, link, _info, _align, entsize in headers:
        name = _cstring(names, name_offset)
        stats.sections[name] = stats.sections.get(name, 0) + size
        body = data[offset:offset + size]
        if kind == SHT_RELA:
            stats.relocation_bytes += size
            for r_offset, r_info, _addend in struct.iter_unpack("<QQq", body[:size - size % 24]):
                stats.relocations += 1
                if r_info & 0xFFFFFFFF != relative:
                    stats.symbol_relocations += 1
                pages.add(r_offset // PAGE)
        elif kind == SHT_RELR:
            stats.relocation_bytes += size
            addresses = _relr_addresses(body[:size - size % 8])
            stats.relocations += len(addresses)
            pages.update(address // PAGE for address in addresses)
        elif name == LAZY_SECTION:
            stats.lazy_needed = [entry.decode("utf-8", "replace") for entry in body.split(b"\0") if entry]
        elif kind == SHT_DYNAMIC:
            strings_header = headers[link]
            strings = data[strings_header[4]:strings_header[4] + strings_header[5]]
            for tag, value in struct.iter_unpack("<qQ", body[:size - size % 16]):
                if tag == DT_NEEDED:
                    stats.needed.append(_cstring(strings, value))
    stats.relocated_bytes = len(pages) * PAGE
    return stats


class Budget:
    def __init__(self, text: str, raises: dict[str, str] | None = None) -> None:
        self.data = tomllib.loads(text)
        self.raises = {name: tomllib.loads(body) for name, body in (raises or {}).items()}

    @property
    def headroom(self) -> float:
        return float(self.data.get("headroom", 0))

    @property
    def architectures(self) -> list[str]:
        return [arch for arch in self.data.get("architectures", []) if isinstance(arch, str)]

    @property
    def targets(self) -> dict[str, int]:
        return {metric: value for metric, value in tapp.ints(self.data.get("targets", {})).items()
                if metric in METRICS}

    @property
    def apps_without_media(self) -> list[str]:
        values = self.data.get("apps_without_media", [])
        return [value for value in values if isinstance(value, str)] if isinstance(values, list) else []

    @property
    def engine_users(self) -> dict[str, list[str]]:
        """The apps of apps_without_media that use each engine library, so may load it."""
        table = self.data.get("engine_users", {})
        if not isinstance(table, dict):
            return {}
        return {library: [app for app in apps if isinstance(app, str)]
                for library, apps in table.items() if isinstance(apps, list)}

    @property
    def needed_absent(self) -> list[str]:
        values = self.data.get("targets", {}).get("needed_absent", [])
        return [value for value in values if isinstance(value, str)] if isinstance(values, list) else []

    def added(self, names: set[str] | None = None) -> dict[tuple[str, str], int]:
        """Bytes or counts the raise files (or only `names`) add, by (architecture, metric)."""
        sums: dict[tuple[str, str], int] = {}
        for name, raised in self.raises.items():
            if names is not None and name not in names:
                continue
            table = raised.get("runtime", {})
            for arch, metrics in (table.items() if isinstance(table, dict) else ()):
                for metric, value in tapp.ints(metrics).items():
                    sums[(arch, metric)] = sums.get((arch, metric), 0) + value
        return sums

    def totals(self) -> dict[tuple[str, str], int]:
        """Each (architecture, metric) budget, its raises included."""
        totals = {(arch, metric): value for arch, table in self.data.get("runtime", {}).items()
                  for metric, value in tapp.ints(table).items()}
        for key, value in self.added().items():
            if key in totals:
                totals[key] += value
        return totals


def load(root: pathlib.Path = REPO) -> Budget:
    directory = root / DIRECTORY
    raises = {path.name: path.read_text() for path in sorted((directory / "raises").glob("*.toml"))}
    return Budget((directory / "budget.toml").read_text(), raises)


def load_at(revision: str, root: pathlib.Path = REPO) -> Budget | None:
    """The budget at `revision`, or None when the revision has none. An unknown revision fails."""
    if tapp.git(root, "rev-parse", "--verify", "--quiet", f"{revision}^{{commit}}").returncode != 0:
        raise ValueError(f"cannot read revision {revision}: fetch it before comparing budgets")
    shown = tapp.git(root, "show", f"{revision}:{DIRECTORY}/budget.toml")
    if shown.returncode != 0:
        return None
    raises = {}
    listed = tapp.git(root, "ls-tree", "--name-only", revision, f"{DIRECTORY}/raises/")
    for path in listed.stdout.split():
        if path.endswith(".toml"):
            raises[pathlib.PurePosixPath(path).name] = tapp.git(root, "show", f"{revision}:{path}").stdout
    return Budget(shown.stdout, raises)


def budget_for(metric: str, value: int, headroom: float) -> int:
    rounding = ROUNDING[metric]
    return max(1, math.ceil(value * (1 + headroom) / rounding)) * rounding


def run_label() -> str:
    number = os.environ.get("GITHUB_RUN_NUMBER", "")
    return number if number.isdigit() else "<the Release / apps run number>"


def describe(stats: ElfStats) -> list[str]:
    """What a person reading the gate needs to see where the bytes are."""
    biggest = sorted(((size, name) for name, size in stats.sections.items() if name), reverse=True)[:8]
    return [f"runtime {stats.arch}: {stats.bytes} bytes; " + ", ".join(
                f"{name} {size / 1e6:.2f} MB" for size, name in biggest),
            f"runtime {stats.arch}: {stats.relocations} dynamic relocations "
            f"({stats.symbol_relocations} with a symbol lookup) in {stats.relocation_bytes} bytes, "
            f"writing {stats.relocated_bytes // PAGE} pages ({stats.relocated_bytes} bytes) per app process",
            f"runtime {stats.arch}: DT_NEEDED " + " ".join(stats.needed),
            f"runtime {stats.arch}: loaded on first media use: " + (" ".join(stats.lazy_needed) or "nothing")]


def judge(stats: ElfStats, budget: Budget) -> tuple[int, list[str]]:
    """One measured dylib against its architecture's budgets and the KPI targets."""
    arch = stats.arch
    if arch not in budget.architectures:
        return 1, [f"error: no runtime budgets for architecture {arch!r}"]
    limits, targets = budget.totals(), budget.targets
    lines = describe(stats)
    over: dict[str, int] = {}
    missing = False
    for metric, value in stats.metrics().items():
        proposed = budget_for(metric, value, budget.headroom)
        limit = limits.get((arch, metric))
        target = targets.get(metric)
        goal = f"; KPI target {target}" if target is not None else ""
        if limit is None:
            missing = True
            lines.append(f"error: runtime {arch} {metric}: {value} and no budget; add "
                         f"[runtime.{arch}] {metric} = {proposed} to {DIRECTORY}/budget.toml")
        elif value > limit:
            over[metric] = proposed - limit
            lines.append(f"error: runtime {arch} {metric}: {value} is over its budget of {limit} "
                         f"(+{value - limit}){goal}")
        elif proposed < limit * (1 - SLACK):
            lines.append(f"runtime {arch} {metric}: {value}, under its budget of {limit}{goal}; lower it: "
                         f"[runtime.{arch}] {metric} = {proposed}, and drop it from its raise files")
        else:
            lines.append(f"runtime {arch} {metric}: {value}, within its budget of {limit}{goal}")
    loaded = [name for name in stats.needed if media(name, budget)]
    if loaded:
        missing = True
        lines.append(f"error: runtime {arch}: every app loads {' '.join(loaded)}; none of "
                     f"{', '.join(budget.needed_absent)} may be DT_NEEDED: link the runtime "
                     "through tools/ci/runtime-dylib-lazy-link.py")
    if over:
        lines += [f"A deliberate growth adds one raise file for the change, {DIRECTORY}/raises/<change>.toml",
                  "(the other architecture's job prints its own lines; docs/measurements/runtime-size.md):",
                  '    reason = "<why the runtime grew, at least 20 characters>"',
                  f"    run = {run_label()}",
                  f"    [runtime.{arch}]"]
        lines += [f"    {metric} = {added}" for metric, added in sorted(over.items())]
    return (1 if over or missing else 0), lines


def media(name: str, budget: Budget) -> bool:
    return any(name.startswith(prefix) for prefix in budget.needed_absent)


def find_library(name: str, arch: str, directories: list[pathlib.Path]) -> tuple[pathlib.Path, ElfStats] | None:
    """The first file called `name` in `directories` that is an ELF object for `arch`."""
    for directory in directories:
        candidate = directory / name
        if not candidate.is_file():
            continue
        try:
            stats = read_elf(candidate)
        except (ValueError, struct.error):
            continue
        if stats.arch == arch:
            return candidate, stats
    return None


def closure(path: pathlib.Path, stats: ElfStats,
            directories: list[pathlib.Path]) -> tuple[dict[str, tuple[ElfStats, str]], list[str]]:
    """Every object the loader maps for `path`, each with what needs it, and what cannot be found."""
    found: dict[str, tuple[ElfStats, str]] = {path.name: (stats, "")}
    queue = [(path.name, stats)]
    problems = []
    while queue:
        parent, parent_stats = queue.pop(0)
        for name in parent_stats.needed:
            if name in found:
                continue
            located = find_library(name, stats.arch, directories)
            if located is None:
                problems.append(f"error: {path.name}: cannot find {name} (needed by {parent}) in "
                                f"{' '.join(str(d) for d in directories)}")
                continue
            found[name] = (located[1], parent)
            queue.append((name, located[1]))
    return found, problems


def closure_lines(path: pathlib.Path, stats: ElfStats, budget: Budget,
                  directories: list[pathlib.Path],
                  engines: frozenset[str] = frozenset()) -> tuple[int, list[str]]:
    """Fail when any object loaded with `path` is a library of `needed_absent` or of `engines`."""
    found, errors = closure(path, stats, [path.parent, *directories])
    for name, (_, parent) in found.items():
        if not media(name, budget) and name not in engines:
            continue
        chain = [name]
        while parent:
            chain.append(parent)
            parent = found[parent][1]
        if name in engines:
            errors.append(f"error: {path.name} loads the engine {name} at exec ({' <- '.join(chain)}); "
                          "an engine library is loaded only by the app that uses it")
        else:
            errors.append(f"error: {path.name} loads {name} at exec ({' <- '.join(chain)}); none of "
                          f"{', '.join(budget.needed_absent)} may be")
    pages = sum(entry[0].relocated_bytes for entry in found.values()) // PAGE
    summary = (f"{path.name}: {len(found)} objects at exec, relocations write {pages} pages "
               f"across them: {' '.join(found)}")
    return (1 if errors else 0), [*errors, summary]


def engine_lines(path: pathlib.Path, runtime: ElfStats, runtime_name: str) -> tuple[int, list[str]]:
    """One engine library's numbers, reported only, and whether it links the runtime."""
    try:
        stats = read_elf(path)
    except (ValueError, struct.error) as error:
        return 1, [f"error: engine {path}: {error}"]
    name = path.name
    if stats.arch != runtime.arch:
        return 1, [f"error: engine {name} is built for {stats.arch}, not {runtime.arch}"]
    lines = [f"engine {name} {stats.arch}: {stats.bytes} bytes, {stats.relocations} dynamic relocations "
             f"({stats.symbol_relocations} with a symbol lookup) in {stats.relocation_bytes} bytes, "
             f"writing {stats.relocated_bytes // PAGE} pages ({stats.relocated_bytes} bytes) in its app's process",
             f"engine {name} {stats.arch}: DT_NEEDED " + " ".join(stats.needed)]
    if runtime_name not in stats.needed:
        return 1, [*lines, f"error: engine {name} does not link {runtime_name}, so it carries its own "
                           "copy of what the runtime shares"]
    return 0, lines


def measure(path: pathlib.Path, budget: Budget, arch: str | None = None, apps: list[pathlib.Path] = (),
            directories: list[pathlib.Path] | None = None,
            engines: list[pathlib.Path] = ()) -> tuple[int, list[str], ElfStats]:
    """The dylib against its budgets; with `directories`, also what it and `apps` load at exec.

    Each of `engines` is reported, and must link the runtime and stay out of what `apps` load."""
    stats = read_elf(path)
    if arch is not None and stats.arch != arch:
        return 1, [f"error: {path} is built for {stats.arch}, not {arch}"], stats
    status, lines = judge(stats, budget)
    for engine in engines:
        checked, more = engine_lines(engine, stats, path.name)
        status |= checked
        lines += more
    if directories is None:
        return status, lines, stats
    # What every app loads with the runtime, built or not.
    checked, more = closure_lines(path, stats, budget, directories)
    status |= checked
    lines += more
    if not apps:
        lines.append("no app of apps_without_media was built; checked what every app loads with the runtime")
    for app in apps:
        try:
            app_stats = read_elf(app)
        except (ValueError, struct.error) as error:
            status = 1
            lines.append(f"error: {app}: {error}")
            continue
        uses = {library for library, users in budget.engine_users.items() if app.name in users}
        checked, more = closure_lines(app, app_stats, budget, [path.parent, *directories],
                                      frozenset(engine.name for engine in engines) - uses)
        status |= checked
        lines += more
    return status, lines, stats


# --- The published runtime -------------------------------------------------

Fetch = Callable[[str], bytes]


def http_get(url: str) -> bytes:
    if not url.startswith("https://"):
        raise ValueError(f"refusing to fetch {url}: not https")
    request = urllib.request.Request(url, headers={"User-Agent": "tilo-perf-runtime-size"})
    with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:  # noqa: S310 (https only)
        return response.read()


def newest_runtimes(entries: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """The newest tilo-os `so.tilo.runtime` version per architecture in a registry index."""
    newest: dict[str, dict[str, Any]] = {}
    for entry in entries.get("entries", []):
        if entry.get("package_id") != RUNTIME_PACKAGE:
            continue
        for version in entry.get("versions", []):
            target = version.get("target", {})
            arch = target.get("architecture")
            if target.get("platform") != "tilo-os" or not isinstance(arch, str):
                continue
            if arch not in newest or version["released_at_unix"] > newest[arch]["released_at_unix"]:
                newest[arch] = version
    if not newest:
        raise ValueError(f"the registry index lists no {RUNTIME_PACKAGE}")
    return newest


def collection_hash(collection: bytes, path: str) -> str:
    """The blob hash of `path` in a runtime package's `collection.qcl` (tilo-app-package blobs.rs)."""
    if collection[:8] != b"TLOCOL\0\0":
        raise ValueError("collection.qcl: bad magic")
    (count,) = struct.unpack_from("<I", collection, 8)
    cursor = 12
    for _ in range(count):
        (length,) = struct.unpack_from("<I", collection, cursor)
        cursor += 4
        name = collection[cursor:cursor + length].decode("utf-8")
        cursor += length
        digest = collection[cursor:cursor + 32]
        cursor += 32
        if len(digest) != 32:
            raise ValueError("collection.qcl: truncated")
        if name == path:
            return digest.hex()
    raise ValueError(f"collection.qcl lists no {path}")


def fetch_blob(digest: str, base: str, get: Fetch, sink: io.BufferedIOBase) -> int:
    """Write blob `digest` from the registry to `sink`, whole or from its chunk list; its length."""
    try:
        body = get(f"{base}/blobs/{digest}")
    except urllib.error.HTTPError as error:
        error.close()
        if error.code != 404:
            raise
    else:
        sink.write(body)
        return len(body)
    listing = json.loads(get(f"{base}/blobs/{digest}.chunks"))
    if listing.get("blob") != digest:
        raise ValueError(f"chunk list for {digest} describes {listing.get('blob')!r}")
    written = 0
    for chunk in listing["chunks"]:
        body = get(f"{base}/blobs/{chunk['hash']}")
        if len(body) != chunk["len"]:
            raise ValueError(f"chunk {chunk['hash']} is {len(body)} bytes, listed as {chunk['len']}")
        sink.write(body)
        written += len(body)
    if written != listing["len"]:
        raise ValueError(f"blob {digest} reassembled to {written} bytes, listed as {listing['len']}")
    return written


def published_dylib(version: dict[str, Any], base: str, get: Fetch, out: pathlib.Path) -> None:
    """Write the dylib of one published runtime package to `out`."""
    with tarfile.open(fileobj=io.BytesIO(get(version["tapp_url"]))) as package:
        member = package.extractfile("collection.qcl")
        if member is None:
            raise ValueError(f"{version['tapp_url']}: no collection.qcl")
        digest = collection_hash(member.read(), DYLIB)
    with out.open("wb") as sink:
        fetch_blob(digest, base, get, sink)


def published(entries: dict[str, Any], budget: Budget, base: str = REGISTRY,
              get: Fetch = http_get) -> tuple[int, list[str], dict[str, dict[str, Any]]]:
    """Each architecture's newest published runtime against its budgets."""
    status = 0
    lines = [f"registry sequence {entries.get('sequence', '?')}"]
    measured: dict[str, dict[str, Any]] = {}
    runtimes = newest_runtimes(entries)
    for arch in budget.architectures:
        version = runtimes.get(arch)
        if version is None:
            status = 1
            lines.append(f"error: {arch}: the registry lists no {RUNTIME_PACKAGE}")
            continue
        with tempfile.TemporaryDirectory() as scratch:
            path = pathlib.Path(scratch) / "libruntime_dylib.so"
            published_dylib(version, base, get, path)
            arch_status, arch_lines, stats = measure(path, budget, arch)
        status |= arch_status
        lines.append(f"{RUNTIME_PACKAGE} {version['version']} ({version['target'].get('runtime', '?')}), {arch}")
        lines += arch_lines
        measured[arch] = {"version": version["version"], "runtime": version["target"].get("runtime", ""),
                          **stats.metrics()}
    return status, lines, measured


# --- The change -------------------------------------------------------------

def shape_problems(budget: Budget) -> list[str]:
    problems = []
    arches = budget.architectures
    if not arches:
        problems.append("budget.toml names no architectures")
    if set(budget.targets) != set(METRICS) or any(value <= 0 for value in budget.targets.values()):
        problems.append(f"[targets] needs a positive KPI target for each of {', '.join(METRICS)}")
    tables = budget.data.get("runtime", {})
    if set(tables) != set(arches):
        problems.append(f"[runtime.<arch>] tables must be exactly {', '.join(arches)}")
    for arch, table in sorted(tables.items()):
        values = tapp.ints(table) if isinstance(table, dict) else {}
        if set(values) != set(METRICS) or len(values) != len(table) or any(v <= 0 for v in values.values()):
            problems.append(f"[runtime.{arch}] needs a positive budget for each of {', '.join(METRICS)}")
    users = budget.data.get("engine_users", {})
    for library, apps in (users.items() if isinstance(users, dict) else [("", None)]):
        if not (library.startswith("lib") and library.endswith(".so") and isinstance(apps, list) and apps
                and all(app in budget.apps_without_media for app in apps)):
            problems.append("[engine_users] maps an engine library (lib*.so) to the apps of "
                            "apps_without_media that use it")
            break
    return problems


def raise_problems(name: str, raised: dict[str, Any], budget: Budget) -> list[str]:
    problems = []
    reason, run = raised.get("reason"), raised.get("run")
    if not (isinstance(reason, str) and len(reason.strip()) >= MIN_REASON
            and isinstance(run, int) and not isinstance(run, bool) and run > 0):
        problems.append(f"raise {name} needs a reason of at least {MIN_REASON} characters "
                        "and the run number that measured the growth")
    unknown = set(raised) - {"reason", "run", "runtime"}
    if unknown:
        problems.append(f"raise {name} has unknown keys {sorted(unknown)}")
    base = budget.data.get("runtime", {})
    table = raised.get("runtime", {})
    adds = 0
    for arch, metrics in (table.items() if isinstance(table, dict) else ()):
        for metric, value in (metrics.items() if isinstance(metrics, dict) else ()):
            adds += 1
            budgeted = base.get(arch, {}).get(metric) if isinstance(base.get(arch), dict) else None
            if not isinstance(budgeted, int):
                problems.append(f"raise {name}: [runtime.{arch}] {metric} names nothing with a budget")
            elif not isinstance(value, int) or isinstance(value, bool) or value <= 0:
                problems.append(f"raise {name}: [runtime.{arch}] {metric} must add a positive amount")
            elif value > budgeted:
                problems.append(f"raise {name}: [runtime.{arch}] {metric} more than doubles its budget")
    if adds == 0:
        problems.append(f"raise {name} adds nothing")
    return problems


def increases(before: Budget, after: Budget, new_raises: set[str]) -> list[str]:
    problems = []
    if after.headroom > before.headroom:
        problems.append("headroom raised")
    for metric, value in after.targets.items():
        if value > before.targets.get(metric, value):
            problems.append(f"KPI target {metric} raised; targets only decrease")
    if set(before.needed_absent) - set(after.needed_absent):
        problems.append("[targets] needed_absent lost an entry; targets only tighten")
    if set(before.apps_without_media) - set(after.apps_without_media):
        problems.append("apps_without_media lost an entry; targets only tighten")
    old, allowance = before.totals(), after.added(new_raises)
    for key, total in sorted(after.totals().items()):
        if key in old and total - old[key] > allowance.get(key, 0):
            problems.append(f"[runtime.{key[0]}] {key[1]}: raised from {old[key]} to {total} beyond the "
                            f"{allowance.get(key, 0)} its new raise files add")
    return problems


def check(before: Budget | None, after: Budget) -> list[str]:
    problems = shape_problems(after)
    new_raises = set(after.raises) - set(before.raises if before is not None else {})
    for name in sorted(new_raises):
        problems += raise_problems(name, after.raises[name], after)
    if before is not None:
        problems += increases(before, after, new_raises)
    return problems


def emit(status: int, lines: list[str], summary: pathlib.Path | None) -> int:
    return tapp.emit(status, lines, summary)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    measured = commands.add_parser("measure", help="a built libruntime_dylib.so against its budgets")
    measured.add_argument("dylib", type=pathlib.Path)
    measured.add_argument("--arch")
    measured.add_argument("--summary", type=pathlib.Path)
    measured.add_argument("--apps-dir", type=pathlib.Path,
                          help="where the built apps are; checks what the runtime and each built app of "
                               "apps_without_media load at exec")
    measured.add_argument("--library-dir", type=pathlib.Path, action="append", default=[],
                          help="a directory the loader searches (the Rust libstd's); repeatable")
    measured.add_argument("--engine", type=pathlib.Path, action="append", default=[],
                          help="a runtime engine library built with it: reported, and kept out "
                               "of what apps_without_media load; repeatable")
    published_parser = commands.add_parser("published", help="the newest published runtimes against their budgets")
    published_parser.add_argument("entries", type=pathlib.Path)
    published_parser.add_argument("--registry", default=REGISTRY)
    published_parser.add_argument("--measurements", type=pathlib.Path)
    published_parser.add_argument("--summary", type=pathlib.Path)
    checked = commands.add_parser("check-budget", help="budgets only decrease without a raise file")
    checked.add_argument("--base", required=True)
    commands.add_parser("apps-without-media", help="the apps budget.toml lists as playing no media, one a line")
    args = parser.parse_args(argv)
    try:
        current = load()
        if args.command == "measure":
            directories = None
            apps: list[pathlib.Path] = []
            if args.apps_dir is not None:
                directories = [*args.library_dir, *SYSTEM_DIRECTORIES]
                apps = [args.apps_dir / name for name in current.apps_without_media
                        if (args.apps_dir / name).is_file()]
            status, lines, _ = measure(args.dylib, current, args.arch, apps, directories, args.engine)
            return emit(status, lines, args.summary)
        if args.command == "apps-without-media":
            print("\n".join(current.apps_without_media))
            return 0
        if args.command == "published":
            entries = json.loads(args.entries.read_text(encoding="utf-8"))
            status, lines, values = published(entries, current, args.registry.rstrip("/"))
            if args.measurements is not None:
                document = {"sequence": entries.get("sequence"), "runtimes": values}
                args.measurements.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
            return emit(status, lines, args.summary)
        problems = check(load_at(args.base), current)
    except (ValueError, KeyError, OSError, struct.error, tarfile.TarError, tomllib.TOMLDecodeError) as error:
        print(f"runtime_size_ratchet.py: {error}", file=sys.stderr)
        return 1
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    if not problems:
        print(f"runtime budgets checked against {args.base}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
