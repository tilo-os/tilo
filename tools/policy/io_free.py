"""The crates that promise to do no I/O, and what their shipped closure links.

A crate listed in `IO_FREE` is pure evaluation over the model's types: who may do what,
decided over plain values. Every layer links it, the toolkit included, so it stays free of the
runtime and the disk. Its shipped closure (normal and build dependencies, with the features
its dependents turn on) reaches no async runtime or network crate, and no workspace crate
whose compiled source (the modules and items those features build) opens a file or a socket,
or starts a process.
"""

from __future__ import annotations

import re
import tomllib
from pathlib import Path

from rust_source import (
    MOD_DECLARATION,
    OUTER_ATTRIBUTE,
    PATH_ATTRIBUTE,
    item_extent,
    lex,
    matching,
    module_children,
    production_source,
    split_top_level,
)

# The crates that promise no I/O, and why each does.
IO_FREE = {
    "tilo-permissions": "who may do what, as a window asks it, which every layer and the toolkit link",
}

# The crates of such a closure that do I/O today, each to cut, are the `io-free` list of
# tools/policy/crate_layout.toml (`crate -> what it links`). The list only shrinks: a crate the
# closure reaches that does I/O and is not listed fails, and so does a listed one it no longer
# reaches.

# External crates that are an async runtime or reach the network.
BANNED = {
    "tokio", "mio", "async-std", "smol", "async-io", "socket2", "hyper", "reqwest", "h2",
    "quinn", "iroh", "rustls", "native-tls", "tungstenite", "tokio-tungstenite", "ureq",
}
# Compiled source that does I/O itself.
IO_USE = re.compile(
    r"\bstd\s*::\s*fs\b|\bstd\s*::\s*\{[^}]*\bfs\b"
    r"|\bstd\s*::\s*net\s*::\s*(?:TcpStream|TcpListener|UdpSocket|ToSocketAddrs)\b"
    r"|\bstd\s*::\s*process\s*::\s*Command\b|\btokio\s*::"
)
CFG = re.compile(r"#\s*\[\s*cfg\s*\(")
INNER_CFG = re.compile(r"#\s*!\s*\[\s*cfg\s*\(")


def holds(predicate: str, features: set[str]) -> bool | None:
    """Whether a `cfg` predicate holds in a production build with `features`; `None` when it
    depends on the target."""
    predicate = re.sub(r"\s+", "", predicate)
    feature = re.fullmatch(r'feature="([^"]*)"', predicate)
    if feature:
        return feature.group(1) in features
    if predicate == "test":
        return False
    call = re.fullmatch(r"(all|any|not)\((.*)\)", predicate)
    if not call:
        return None
    values = [holds(argument, features) for argument in split_top_level(call.group(2))]
    if call.group(1) == "not":
        return None if values[0] is None else not values[0]
    if call.group(1) == "all":
        return False if False in values else (True if all(values) else None)
    return True if True in values else (False if all(value is False for value in values) else None)


def compiled(text: str, features: set[str]) -> str:
    """The code of `text` a build with `features` compiles: comments, literals, test items and
    the items whose `cfg` cannot hold blanked."""
    code = production_source(lex(text)[0])
    inner = INNER_CFG.search(code)
    if inner and not code[: inner.start()].strip():
        end = matching(code, inner.end() - 1)
        if holds(text[inner.end(): end - 1], features) is False:
            return ""
    out = list(code)
    for attribute in CFG.finditer(code):
        if code[attribute.start() + 1: attribute.start() + 3].lstrip().startswith("!"):
            continue
        end = matching(code, attribute.end() - 1)
        if holds(text[attribute.end(): end - 1], features) is not False:
            continue
        _, item_end = item_extent(code, attribute.start())
        for i in range(attribute.start(), item_end):
            if out[i] != "\n":
                out[i] = " "
    return "".join(out)


def modules(root: Path, features: set[str]) -> list[tuple[Path, str]]:
    """Each file of the crate at `root` a build with `features` compiles, with its code."""
    lib = root / "src" / "lib.rs"
    found = []
    stack = [lib] if lib.is_file() else []
    while stack:
        file = stack.pop()
        text = file.read_text()
        code = compiled(text, features)
        found.append((file, code))
        children = module_children(file)
        for declared in MOD_DECLARATION.finditer(code):
            before = text[max(0, declared.start() - 200): declared.start()]
            path = PATH_ATTRIBUTE.findall(before[before.rfind(";") + 1:])
            if path:
                stack.append((file.parent / path[-1]).resolve())
                continue
            name = declared.group(1)
            for candidate in (children / f"{name}.rs", children / name / "mod.rs"):
                if candidate.is_file():
                    stack.append(candidate)
                    break
    return found


def _dependencies(manifest: dict) -> list[tuple[str, dict]]:
    tables = [manifest.get("dependencies") or {}, manifest.get("build-dependencies") or {}]
    for target in (manifest.get("target") or {}).values():
        tables += [target.get("dependencies") or {}, target.get("build-dependencies") or {}]
    return [(key, spec if isinstance(spec, dict) else {"version": spec})
            for table in tables for key, spec in table.items()]


class Workspace:
    def __init__(self, root: Path):
        self.root = root
        document = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
        self.shared = document.get("dependencies", {})
        self.manifests: dict[str, tuple[Path, dict]] = {}
        self.by_path: dict[Path, str] = {}
        for pattern in document["members"]:
            for path in sorted(root.glob(pattern)):
                manifest = path / "Cargo.toml"
                if manifest.is_file():
                    data = tomllib.loads(manifest.read_text())
                    self.manifests[data["package"]["name"]] = (path, data)
                    self.by_path[path.resolve()] = data["package"]["name"]

    def _resolve(self, home: Path, key: str, spec: dict) -> tuple[str, dict]:
        if spec.get("workspace"):
            shared = self.shared.get(key, {})
            shared = shared if isinstance(shared, dict) else {"version": shared}
            spec = {**shared, "features": [*shared.get("features", []), *spec.get("features", [])]}
            home = self.root
        if "path" in spec:
            return self.by_path.get((home / spec["path"]).resolve(), spec.get("package", key)), spec
        return spec.get("package", key), spec

    def closure(self, start: str) -> dict[str, set[str]]:
        """Every package `start` ships, with the features turned on in it."""
        asked: dict[str, set[str]] = {}
        on: dict[str, set[str]] = {}
        queue = [(start, {"default"})]
        while queue:
            package, features = queue.pop()
            if package in asked and features <= asked[package]:
                continue
            asked.setdefault(package, set()).update(features)
            if package not in self.manifests:
                on[package] = asked[package]
                continue
            home, manifest = self.manifests[package]
            table = manifest.get("features", {}) or {}
            active: set[str] = set()
            wanted: dict[str, set[str]] = {}
            weak: list[tuple[str, str]] = []
            expanded: set[str] = set()
            pending = list(asked[package])
            while pending:
                feature = pending.pop()
                if feature in expanded:
                    continue
                expanded.add(feature)
                if feature not in table:
                    active.add(feature)
                for entry in table.get(feature, []):
                    if entry.startswith("dep:"):
                        active.add(entry[4:])
                    elif "?/" in entry:
                        weak.append(tuple(entry.split("?/", 1)))
                    elif "/" in entry:
                        dependency, sub = entry.split("/", 1)
                        active.add(dependency)
                        wanted.setdefault(dependency, set()).add(sub)
                    else:
                        pending.append(entry)
            for dependency, sub in weak:
                if dependency in active:
                    wanted.setdefault(dependency, set()).add(sub)
            on[package] = expanded
            for key, spec in _dependencies(manifest):
                if spec.get("optional") and key not in active:
                    continue
                name, merged = self._resolve(home, key, spec)
                if name == "workspace-hack":
                    continue
                features = set(merged.get("features", [])) | wanted.get(key, set())
                if merged.get("default-features", True):
                    features.add("default")
                queue.append((name, features))
        return on

    def doing_io(self, start: str) -> dict[str, str]:
        """Each package of `start`'s closure that does I/O, with where."""
        found = {}
        for package, features in sorted(self.closure(start).items()):
            if package == start and package not in self.manifests:
                continue
            if package in BANNED:
                found[package] = "an async runtime or a network crate"
            elif package in self.manifests:
                home, manifest = self.manifests[package]
                # A proc macro runs in the compiler; nothing of it ships.
                if (manifest.get("lib") or {}).get("proc-macro"):
                    continue
                for file, code in modules(home, features):
                    if IO_USE.search(code):
                        found[package] = str(file.relative_to(self.root))
                        break
        return found


def check(root: Path, listed: set[str]) -> list[str]:
    workspace = Workspace(root)
    problems = []
    for crate, why in IO_FREE.items():
        found = workspace.doing_io(crate)
        known = {edge.split(" -> ")[1] for edge in listed if edge.split(" -> ")[0] == crate}
        for package, where in sorted(found.items()):
            if package not in known:
                problems.append(
                    f"{crate} links {package}, which does I/O ({where}).\n"
                    f"  why: {crate} promises no I/O: {why}.\n"
                    f"  fix: take only the I/O-free types it needs from {package}, moving them down\n"
                    f"  into an I/O-free crate, or turn off the feature that brings the I/O in."
                )
        for package in sorted(known - set(found)):
            problems.append(
                f"tools/policy/crate_layout.toml: `{crate} -> {package}` under [io-free] is no longer\n"
                f"  an I/O crate {crate} links: delete the line"
            )
    return problems
