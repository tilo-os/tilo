#!/usr/bin/env python3
"""Select and run the hermetic test suites that live outside Cargo.

tools/ci/suites.toml declares each suite with the repository paths it reads.
t.toml has one `suite-<group>` check per group, which runs it with `run` when
the group's inputs change (tools/ci/t_toml.py writes them). The same manifest declares
the Cargo packages whose code reads repository files outside the package, so
a change to those files selects the package's checks and tests. `check` is the
repository policy that keeps every test-shaped file and every cross-package
read accounted for.
"""

from __future__ import annotations

import argparse
import bisect
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
import fnmatch
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import tomllib

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "tools/ci/suites.toml"
SECURITY_MANIFEST = "tools/security-boundary-invariants.txt"
# Files whose change can alter which suites run or how, so every suite runs.
CONTROL_INPUTS = frozenset({MANIFEST, "tools/ci/suites.py", "tools/tests/test_suites.py"})
# Always-run gates: a suite referenced here already runs on every applicable change.
ALWAYS_RUN_SOURCES = ("tools/policy/run.sh", "tools/policy/steps.toml")
# Test-shaped files outside these directories must be classified. Python tests
# in tools/tests and .codex/hooks are discovered by tools/policy/run.sh.
DISCOVERED_PYTHON_DIRS = ("tools/tests", ".codex/hooks")
TEST_NAME_PATTERNS = (
    "test_*.py", "*_test.py", "*_test.sh", "test-*.py", "test-*.sh", "*-test.sh",
    "*_invariants.sh", "*.test.mjs",
)
SUITE_TIMEOUT_SECONDS = 600


@dataclass(frozen=True)
class Group:
    name: str
    inputs: tuple[str, ...]
    suites: tuple[str, ...]
    # The cores and memory its t check must be able to pass with (`cores`, `memory`), if it says.
    requirement: tuple[int, str] | None = None


@dataclass(frozen=True)
class ReaderScope:
    """The tests that observe some of a package's outside inputs.

    When a change touches only these inputs, only `filter` within the `test`
    target can see it, so the package's other tests have nothing new to say.
    """
    inputs: tuple[str, ...]
    test: str
    filter: str


@dataclass(frozen=True)
class CargoReader:
    """A Cargo package whose code reads repository files outside its directory."""
    package: str
    inputs: tuple[str, ...]
    readers: tuple[str, ...]
    scope: ReaderScope | None = None
    # Directories whose workspace packages' local dependencies the package also reads (their
    # sources, as a test of every app reads the crates the apps use); tools/ci/t_toml.py adds
    # those packages' directories to the inputs from the workspace's metadata.
    dependencies_of: tuple[str, ...] = ()


@dataclass(frozen=True)
class Manifest:
    groups: tuple[Group, ...]
    covered: dict[str, str]
    unrequired: dict[str, str]
    cargo: tuple[CargoReader, ...] = ()
    cargo_exempt: dict[str, str] = field(default_factory=dict)

    def suites(self) -> list[str]:
        return sorted(suite for group in self.groups for suite in group.suites)


def _strings(value: object, where: str) -> tuple[str, ...]:
    if not isinstance(value, list) or not value or not all(isinstance(item, str) and item for item in value):
        raise ValueError(f"{where} must be a non-empty list of strings")
    return tuple(value)


def _path(value: object, where: str) -> str:
    if (not isinstance(value, str) or not value or value.startswith("/") or "\\" in value
            or ".." in value.split("/") or re.search(r"\s", value)):
        raise ValueError(f"{where} is not a repository-relative path: {value!r}")
    return value


def parse(text: str) -> Manifest:
    """Parse the manifest strictly: unknown keys and duplicate suites are errors."""
    data = tomllib.loads(text)
    unknown = set(data) - {"group", "covered", "unrequired", "cargo", "cargo-exempt"}
    if unknown:
        raise ValueError(f"unknown manifest tables: {', '.join(sorted(unknown))}")
    groups: list[Group] = []
    seen: dict[str, str] = {}

    def claim(suite: str, owner: str) -> None:
        if suite in seen:
            raise ValueError(f"{suite} is listed twice ({seen[suite]} and {owner})")
        seen[suite] = owner

    names: set[str] = set()
    for index, entry in enumerate(data.get("group", [])):
        where = f"group {index + 1}"
        if not isinstance(entry, dict) or not {"name", "inputs", "suites"} <= set(entry) <= {
                "name", "inputs", "suites", "requirement"}:
            raise ValueError(f"{where} must have exactly name, inputs and suites, and may have a requirement")
        name = entry["name"]
        if not isinstance(name, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", name) or name in names:
            raise ValueError(f"{where} has an invalid or duplicate name: {name!r}")
        names.add(name)
        inputs = tuple(_path(item, f"{where} input") for item in _strings(entry["inputs"], f"{where} inputs"))
        suites = tuple(_path(item, f"{where} suite") for item in _strings(entry["suites"], f"{where} suites"))
        for suite in suites:
            if not suite.endswith((".py", ".sh", ".test.mjs")):
                raise ValueError(f"{suite}: suites must be .py, .sh or .test.mjs files")
            claim(suite, f"group {name}")
        requirement = None
        if "requirement" in entry:
            value = entry["requirement"]
            if (not isinstance(value, dict) or set(value) != {"cores", "memory"}
                    or not isinstance(value["cores"], int) or isinstance(value["cores"], bool) or value["cores"] < 1
                    or not isinstance(value["memory"], str) or not re.fullmatch(r"\d+(\.\d+)?[KMGT]iB", value["memory"])):
                raise ValueError(f"{where} requirement must be {{ cores = N, memory = \"8GiB\" }}")
            requirement = (value["cores"], value["memory"])
        groups.append(Group(name, inputs, suites, requirement))
    covered: dict[str, str] = {}
    for entry in data.get("covered", []):
        if not isinstance(entry, dict) or set(entry) != {"suite", "by"}:
            raise ValueError("covered entries must have exactly suite and by")
        suite = _path(entry["suite"], "covered suite")
        claim(suite, "covered")
        covered[suite] = _path(entry["by"], f"{suite} covering suite")
    unrequired: dict[str, str] = {}
    for entry in data.get("unrequired", []):
        if not isinstance(entry, dict) or not {"suite", "reason"} <= set(entry) <= {"suite", "reason", "workflow"}:
            raise ValueError("unrequired entries must have suite and reason, and optionally workflow")
        suite = _path(entry["suite"], "unrequired suite")
        reason = entry["reason"]
        if not isinstance(reason, str) or not reason.strip():
            raise ValueError(f"{suite}: unrequired entries need a reason")
        claim(suite, "unrequired")
        unrequired[suite] = reason
    cargo: list[CargoReader] = []
    packages: set[str] = set()
    for index, entry in enumerate(data.get("cargo", [])):
        where = f"cargo entry {index + 1}"
        if (not isinstance(entry, dict)
                or not {"package", "inputs", "readers"} <= set(entry)
                <= {"package", "inputs", "readers", "scope", "dependencies_of"}):
            raise ValueError(f"{where} must have package, inputs and readers, and may have a scope "
                             "and dependencies_of")
        package = entry["package"]
        if not isinstance(package, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", package) or package in packages:
            raise ValueError(f"{where} has an invalid or duplicate package: {package!r}")
        packages.add(package)
        inputs = tuple(_path(item, f"{where} input") for item in _strings(entry["inputs"], f"{where} inputs"))
        readers = tuple(_path(item, f"{where} reader") for item in _strings(entry["readers"], f"{where} readers"))
        for reader in readers:
            if not reader.endswith(".rs"):
                raise ValueError(f"{reader}: readers must be .rs files")
            claim(reader, f"cargo {package}")
        scope = None
        if "scope" in entry:
            raw = entry["scope"]
            if not isinstance(raw, dict) or set(raw) != {"inputs", "test", "filter"}:
                raise ValueError(f"{where} scope must have exactly inputs, test and filter")
            scoped = _strings(raw["inputs"], f"{where} scope inputs")
            if not set(scoped) <= set(inputs):
                raise ValueError(f"{where} scope inputs must be a subset of its inputs")
            test, test_filter = raw["test"], raw["filter"]
            if not isinstance(test, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", test):
                raise ValueError(f"{where} scope test must name one test target")
            if not isinstance(test_filter, str) or not test_filter.strip() or "\n" in test_filter:
                raise ValueError(f"{where} scope filter must be a one-line nextest filterset")
            scope = ReaderScope(scoped, test, test_filter)
        dependencies_of = ()
        if "dependencies_of" in entry:
            dependencies_of = tuple(_path(item, f"{where} dependencies_of")
                                    for item in _strings(entry["dependencies_of"], f"{where} dependencies_of"))
            if not all(item.endswith("/") for item in dependencies_of):
                raise ValueError(f"{where} dependencies_of must name directories (ending in /)")
        cargo.append(CargoReader(package, inputs, readers, scope, dependencies_of))
    cargo_exempt: dict[str, str] = {}
    for entry in data.get("cargo-exempt", []):
        if not isinstance(entry, dict) or set(entry) != {"file", "reason"}:
            raise ValueError("cargo-exempt entries must have exactly file and reason")
        path = _path(entry["file"], "cargo-exempt file")
        reason = entry["reason"]
        if not isinstance(reason, str) or not reason.strip():
            raise ValueError(f"{path}: cargo-exempt entries need a reason")
        claim(path, "cargo-exempt")
        cargo_exempt[path] = reason
    return Manifest(tuple(groups), covered, unrequired, tuple(cargo), cargo_exempt)


def load(root: Path = ROOT) -> Manifest:
    return parse((root / MANIFEST).read_text(encoding="utf-8"))


def matches(path: str, pattern: str) -> bool:
    if pattern.endswith("/"):
        return path.startswith(pattern)
    if any(char in pattern for char in "*?["):
        return fnmatch.fnmatchcase(path, pattern)
    return path == pattern


def select(manifest: Manifest, changes: list[str], *, run_all: bool = False) -> list[str]:
    """Return the suites a change must run, in stable order."""
    if run_all or any(path in CONTROL_INPUTS for path in changes):
        return manifest.suites()
    selected = {
        suite
        for group in manifest.groups
        if any(matches(path, pattern) for path in changes for pattern in group.inputs)
        for suite in group.suites
    }
    selected.update(suite for suite in manifest.suites() if suite in changes)
    return sorted(selected)


def select_packages(manifest: Manifest, changes: list[str]) -> list[str]:
    """Return the Cargo packages that read a changed path from outside themselves.

    A package's own files select it through the dependency graph; this adds
    the packages whose tests or build read the changed file from elsewhere.
    """
    return sorted(
        entry.package
        for entry in manifest.cargo
        if any(matches(path, pattern) for path in changes for pattern in entry.inputs)
    )


def select_package_scopes(manifest: Manifest, changes: list[str]) -> dict[str, ReaderScope | None]:
    """Like select_packages, with the narrowest tests that observe the change.

    A package maps to its scope when every changed path among its inputs is a
    scoped input, and to None (its whole suite) otherwise.
    """
    selected: dict[str, ReaderScope | None] = {}
    for entry in manifest.cargo:
        touched = [path for path in changes
                   if any(matches(path, pattern) for pattern in entry.inputs)]
        if not touched:
            continue
        scope = entry.scope
        if scope is not None and all(
                any(matches(path, pattern) for pattern in scope.inputs) for path in touched):
            selected[entry.package] = scope
        else:
            selected[entry.package] = None
    return dict(sorted(selected.items()))


def command(suite: str) -> list[str]:
    # Every suite runs without the desktop session of whoever runs it, as the
    # Rust tests do (tools/cargo-runner.sh).
    if suite.endswith(".py"):
        run = ["python3", suite]
    elif suite.endswith(".mjs"):
        run = ["node", "--test", suite]
    else:
        run = ["bash", suite]
    return ["bash", str(ROOT / "tools/with-test-environment.sh"), *run]


def run(root: Path, suites: list[str], jobs: int) -> int:
    known = set(load(root).suites())
    unknown = [suite for suite in suites if suite not in known]
    if unknown:
        print(f"not declared in {MANIFEST}: {', '.join(unknown)}", file=sys.stderr)
        return 2
    if not suites:
        print("no suites selected", file=sys.stderr)
        return 2

    def one(suite: str) -> tuple[str, int, float, str]:
        started = time.monotonic()
        try:
            result = subprocess.run(command(suite), cwd=root, stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                    text=True, errors="replace", timeout=SUITE_TIMEOUT_SECONDS)
            code, output = result.returncode, result.stdout
        except subprocess.TimeoutExpired as error:
            code = 124
            # What the suite printed before it hung: bytes even under text=True.
            partial = error.stdout or b""
            output = partial.decode(errors="replace") if isinstance(partial, bytes) else partial
            output += f"\ntimed out after {SUITE_TIMEOUT_SECONDS}s\n"
        return suite, code, time.monotonic() - started, output

    failed: list[str] = []
    started = time.monotonic()
    with ThreadPoolExecutor(max_workers=max(1, jobs)) as pool:
        for suite, code, elapsed, output in pool.map(one, suites):
            status = "ok" if code == 0 else f"FAILED (exit {code})"
            print(f"::group::{suite}: {status} in {elapsed:.1f}s", flush=True)
            print(output.rstrip(), flush=True)
            print("::endgroup::", flush=True)
            if code:
                failed.append(suite)
    print(f"{len(suites) - len(failed)}/{len(suites)} suites passed in {time.monotonic() - started:.1f}s")
    for suite in failed:
        print(f"::error::{suite} failed")
    return 1 if failed else 0


def in_scope(path: str) -> bool:
    """Whether a tracked file looks like a test that must be classified."""
    if path.startswith("vendor/"):
        return False
    directory, _, name = path.rpartition("/")
    if directory == "os/tests" and name.endswith((".py", ".sh")):
        return True
    if directory == "tools/tests" and name.endswith(".sh"):
        return True
    if directory in DISCOVERED_PYTHON_DIRS and name.endswith(".py"):
        return False
    return any(fnmatch.fnmatchcase(name, pattern) for pattern in TEST_NAME_PATTERNS)


def always_run(root: Path) -> set[str]:
    """Suites that already run on every applicable change."""
    gated = {
        line.strip() for line in (root / SECURITY_MANIFEST).read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    }
    for source in ALWAYS_RUN_SOURCES:
        text = (root / source).read_text(encoding="utf-8")
        if source.endswith(".toml"):
            # The policy steps' commands, not the files they list as inputs.
            text = "\n".join(step["run"] for step in tomllib.loads(text).get("step", []))
        gated.update(re.findall(r"[A-Za-z0-9_./-]+\.(?:py|sh)\b", text))
    return gated


def audit(root: Path, tracked: list[str], manifest: Manifest | None = None) -> list[str]:
    """Return policy violations for the manifest against the tracked tree."""
    manifest = manifest or load(root)
    problems: list[str] = []
    tracked_set = set(tracked)
    gated = always_run(root) | {
        path for path in tracked
        if path.rpartition("/")[0] in DISCOVERED_PYTHON_DIRS
        and fnmatch.fnmatchcase(path.rpartition("/")[2], "test_*.py")
    }
    required = set(manifest.suites())
    for suite in sorted(required | set(manifest.covered) | set(manifest.unrequired)):
        if suite not in tracked_set:
            problems.append(f"{suite}: listed in {MANIFEST} but not a tracked file")
        elif suite in gated:
            problems.append(f"{suite}: already runs on every change; remove it from {MANIFEST}")
    for suite in sorted(required & tracked_set):
        if suite.endswith(".py"):
            text = (root / suite).read_text(encoding="utf-8")
            if re.search(r"\bTestCase\b", text) and "unittest.main(" not in text:
                problems.append(f"{suite}: defines unittest cases but never calls unittest.main()")
    # Cargo's automatically discovered integration targets are gates too.
    # Require a tracked package manifest, an actual test, and an explicit
    # reader declaration so covered drivers also participate in selection.
    packages = {name: directory for directory, name in cargo_packages(root, tracked).items()}
    for entry in manifest.cargo:
        directory = packages.get(entry.package)
        if directory is None:
            continue
        cargo = tomllib.loads((root / directory / "Cargo.toml").read_text(encoding="utf-8"))
        if cargo.get("package", {}).get("autotests", True) is False:
            continue
        for reader in entry.readers:
            if (reader in tracked_set
                    and Path(reader).parent.as_posix() == f"{directory}/tests"
                    and re.search(r"#\[test\]", (root / reader).read_text(encoding="utf-8"))):
                gated.add(reader)
    for suite, by in sorted(manifest.covered.items()):
        if by not in gated and by not in required:
            problems.append(f"{suite}: covering suite {by} is not itself gated")
        elif by not in tracked_set or not (root / by).is_file():
            problems.append(f"{suite}: covering suite {by} is not a tracked file on disk")
        elif not re.search(
                rf"\b{re.escape(Path(suite).stem)}\b", (root / by).read_text(encoding="utf-8")):
            problems.append(f"{suite}: {by} does not reference it")
    classified = gated | required | set(manifest.covered) | set(manifest.unrequired)
    for path in sorted(tracked):
        if in_scope(path) and path not in classified:
            problems.append(
                f"{path}: test-shaped file is neither gated nor listed in {MANIFEST} "
                "(add it to a group, or to [[covered]]/[[unrequired]] with a reason)")
    problems.extend(audit_cargo(root, tracked, manifest))
    return problems


# How Rust code reaches a repository file outside its package: a literal
# include!/include_str!/include_bytes! or #[path] module whose path climbs out
# of the package, or a path built from CARGO_MANIFEST_DIR that climbs out of it
# in the same statement. A statement that only names target/ writes build
# output instead.
RUST_INCLUDE = re.compile(
    r'\binclude(?:_str|_bytes)?!\s*\(\s*"([^"\\]+)"\s*\)|#\[\s*path\s*=\s*"([^"\\]+)"\s*\]')
MANIFEST_DIR = re.compile(r'env!\s*\(\s*"CARGO_MANIFEST_DIR"\s*\)')
CLIMB = re.compile(r'"/?\.\.(?:/|")|\.parent\(\)|\.ancestors\(\)')
BUILD_OUTPUT = re.compile(r'"(?:\.\./|/)*target(?:/|")')
STATEMENT_END = re.compile(r"[;{}]")


# A step of a path expression that keeps it a path the audit can follow: a
# literal join, a climb, or a conversion that leaves the path as it is.
PATH_STEP = re.compile(
    r'\s*(?:\.join\(\s*"(?P<join>[^"\\]*)"\s*\)|\.ancestors\(\)\s*\.nth\(\s*(?P<nth>\d+)\s*\)'
    r'|(?P<parent>\.parent\(\))|\.(?:canonicalize|unwrap|to_path_buf|to_owned|as_path|clone)\(\)'
    r'|\.expect\(\s*"[^"\\]*"\s*\)|\?|\))')
# A path from CARGO_MANIFEST_DIR bound to a name (`let root = Path::new(env!(…))…;`), or returned
# by a function that takes nothing (`fn root() -> PathBuf { Path::new(env!(…))… }`).
LET_BINDING = re.compile(r'let\s+(?:mut\s+)?(\w+)\s*(?::[^=;]+)?=\s*(?:&\s*)?(?:[\w:]+\(\s*)*$')
FN_BINDING = re.compile(r'fn\s+(\w+)\s*\(\s*\)\s*->\s*[^{;]+\{\s*(?:[\w:]+\(\s*)*$')
# A directory listing whose argument starts here.
READ_DIR_OF = re.compile(r'read_dir\(\s*&?\s*(?:[\w:]+\(\s*)*$')


@dataclass(frozen=True)
class CrossRead:
    line: int
    # The repository path a literal include, or a path built from
    # CARGO_MANIFEST_DIR out of literals, resolves to; None for a runtime path
    # the audit can't follow, whose target the manifest's inputs must describe.
    target: str | None
    # For a directory the code lists: the names it joins onto each entry (empty
    # if none), which the inputs must cover for every entry the tree has.
    each: tuple[str, ...] | None = None
    # Whether the path is built at run time, where a path the tree doesn't hold is one the code
    # makes (a scratch file) or tolerates missing, rather than one it compiles in.
    runtime: bool = False


def _climb(path: str, levels: int) -> str | None:
    parts = [part for part in path.split("/") if part]
    return None if levels > len(parts) else "/".join(parts[:len(parts) - levels])


def _join(path: str, literal: str) -> str | None:
    if literal.startswith("/"):
        return None
    joined = os.path.normpath(os.path.join(path or ".", literal))
    return None if joined == ".." or joined.startswith("../") else ("" if joined == "." else joined)


def _follow(path: str, text: str, start: int) -> tuple[str | None, int, bool]:
    """Apply the path steps at text[start:] to `path`: the path they reach (None once it leaves
    the repository), where they end, and whether any of them joined a name."""
    position, joined = start, False
    while path is not None:
        step = PATH_STEP.match(text, position)
        if step is None or step.end() == position:
            break
        position = step.end()
        if step["join"] is not None:
            path, joined = _join(path, step["join"]), True
        elif step["nth"] is not None:
            path = _climb(path, int(step["nth"]))
        elif step["parent"]:
            path = _climb(path, 1)
    return path, position, joined


def _block_end(text: str, start: int) -> int:
    """The end of the block that encloses `start`."""
    depth = 0
    for index in range(start, len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth < 0:
                return index
    return len(text)


def _joined_names(text: str, start: int) -> tuple[str, ...]:
    """The relative names joined onto some path between `start` and the end of its block: what
    code that lists a directory reads in each entry."""
    rest = text[start:_block_end(text, start)]
    return tuple(sorted({name for name in re.findall(r'\.join\(\s*"([^"\\]+)"\s*\)', rest)
                         if not name.startswith("/") and ".." not in name.split("/")}))


def _statement_start(text: str, position: int) -> int:
    return max(text.rfind(";", 0, position), text.rfind("{", 0, position), text.rfind("}", 0, position)) + 1


def runtime_reads(text: str, directory: str, match: re.Match[str]) -> list[tuple[int, str, tuple[str, ...] | None]]:
    """What a path built from the CARGO_MANIFEST_DIR at `match` reads, as far as literals say: the
    path itself where literals are joined onto it, and through the names it is bound to, each
    literal join onto them and each directory listed (with the names joined onto its entries).
    Returns (offset, path, each) triples; an empty list when nothing can be resolved."""
    concat = re.compile(r'\s*,\s*"([^"\\]*)"').match(text, match.end())
    base: str | None = directory
    position = match.end()
    if concat:
        base, position = _join(directory, concat[1].lstrip("/")), concat.end()
    path, end, joined = _follow(base, text, position) if base is not None else (None, position, False)
    if path is None:
        return []
    found: list[tuple[int, str, tuple[str, ...] | None]] = []
    statement = _statement_start(text, match.start())
    prefix = text[statement:match.start()]
    if READ_DIR_OF.search(prefix):
        found.append((match.start(), path, _joined_names(text, end)))
    elif joined or concat:
        found.append((match.start(), path, None))
    bindings: list[tuple[str, str, int, int]] = []
    whole = re.match(r"\s*;", text[end:]) is not None
    if whole and (binding := LET_BINDING.search(prefix)):
        bindings.append((rf"\b{binding[1]}\b(?!\s*[(!:])", path, end, _block_end(text, end)))
    elif (re.fullmatch(r"\s*(?:[\w:]+\(\s*)*", prefix) and re.match(r"\s*}", text[end:])
          and (function := FN_BINDING.search(text, max(0, statement - 200), match.start()))):
        bindings.append((rf"\b{function[1]}\(\)", path, 0, len(text)))
    seen: set[tuple[str, int]] = set()
    while bindings:
        pattern, bound, scope_start, scope_end = bindings.pop()
        if (pattern, scope_start) in seen:
            continue
        seen.add((pattern, scope_start))
        for use in re.finditer(pattern, text[:scope_end]):
            if use.start() < scope_start or text[use.start() - 1:use.start()] == ".":
                continue
            target, use_end, use_joined = _follow(bound, text, use.end())
            if target is None:
                continue
            if READ_DIR_OF.search(text, max(0, use.start() - 80), use.start()):
                found.append((use.start(), target, _joined_names(text, use_end)))
            elif use_joined:
                found.append((use.start(), target, None))
            alias = LET_BINDING.search(text[_statement_start(text, use.start()):use.start()])
            if alias and re.match(r"\s*;", text[use_end:]):
                bindings.append((rf"\b{alias[1]}\b(?!\s*[(!:])", target, use_end, _block_end(text, use_end)))
    return found


def cargo_packages(root: Path, tracked: list[str]) -> dict[str, str]:
    """Map each tracked package directory ("" for the root) to its package name."""
    packages: dict[str, str] = {}
    for path in tracked:
        if path.rpartition("/")[2] != "Cargo.toml" or path.startswith("vendor/"):
            continue
        try:
            package = tomllib.loads((root / path).read_text(encoding="utf-8")).get("package")
        except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
            raise ValueError(f"cannot audit Cargo package {path}: {error}") from error
        if isinstance(package, dict) and isinstance(package.get("name"), str):
            packages[path.rpartition("/")[0]] = package["name"]
    return packages


def package_dir(path: str, packages: dict[str, str]) -> str | None:
    """The directory of the innermost package that owns `path`."""
    directory = path
    while directory:
        directory = directory.rpartition("/")[0]
        if directory in packages:
            return directory
    return None


def cross_package_reads(root: Path, tracked: list[str],
                        packages: dict[str, str]) -> dict[str, list[CrossRead]]:
    """Rust files that read repository files outside their own package."""
    found: dict[str, list[CrossRead]] = {}
    for path in tracked:
        if not path.endswith(".rs") or path.startswith("vendor/"):
            continue
        directory = package_dir(path, packages)
        if directory is None:
            continue
        text = (root / path).read_text(encoding="utf-8", errors="replace")
        if "include" not in text and "path" not in text and "CARGO_MANIFEST_DIR" not in text:
            continue
        reads: list[CrossRead] = []
        for match in RUST_INCLUDE.finditer(text):
            literal = match.group(1) or match.group(2)
            target = os.path.normpath(os.path.join(path.rpartition("/")[0], literal))
            inside = (target + "/").startswith(directory + "/") if directory else not target.startswith("..")
            if not inside:
                reads.append(CrossRead(text.count("\n", 0, match.start()) + 1, target))
        for match in MANIFEST_DIR.finditer(text):
            statement = STATEMENT_END.split(text[match.end():match.end() + 800], maxsplit=1)[0]
            if CLIMB.search(statement) and not BUILD_OUTPUT.search(statement):
                reads.append(CrossRead(text.count("\n", 0, match.start()) + 1, None))
                for offset, target, each in runtime_reads(text, directory, match):
                    inside = (target + "/").startswith(directory + "/") if directory else True
                    if target and not inside and target.split("/")[0] != "target":
                        reads.append(CrossRead(text.count("\n", 0, offset) + 1, target, each, runtime=True))
        if reads:
            found[path] = reads
    return found


def audit_cargo(root: Path, tracked: list[str], manifest: Manifest) -> list[str]:
    """Every Rust file that reads outside its package must be declared.

    A declared package lists the files that read (so a new reader cannot go
    unnoticed) and the inputs they read (which select the package when they
    change). Literal includes are checked against the inputs, and so are the
    paths built at run time from CARGO_MANIFEST_DIR and literals (runtime_reads);
    for the rest the declaration is trusted.
    """
    problems: list[str] = []
    tracked_set = set(tracked)
    packages = cargo_packages(root, tracked)
    names = set(packages.values())
    reads = cross_package_reads(root, tracked, packages)
    files = Tree(tracked)
    entries = {entry.package: entry for entry in manifest.cargo}
    for entry in manifest.cargo:
        if entry.package not in names:
            problems.append(f"{MANIFEST}: cargo {entry.package}: no tracked Cargo.toml declares this package")
        for pattern in entry.inputs:
            if not any(matches(path, pattern) for path in tracked):
                problems.append(f"{MANIFEST}: cargo {entry.package}: input {pattern} matches no tracked file")
        for reader in entry.readers:
            directory = package_dir(reader, packages)
            if reader not in tracked_set:
                problems.append(f"{reader}: listed in {MANIFEST} but not a tracked file")
            elif directory is None or packages[directory] != entry.package:
                problems.append(f"{reader}: listed as a reader of {entry.package}, which does not own it")
            elif reader not in reads:
                problems.append(f"{reader}: no longer reads outside {entry.package}; remove it from {MANIFEST}")
    for path, reason in sorted(manifest.cargo_exempt.items()):
        if path not in tracked_set:
            problems.append(f"{path}: listed in {MANIFEST} but not a tracked file")
        elif path not in reads:
            problems.append(f"{path}: cargo-exempt, but reads nothing outside its package; remove it")
    for path, found in sorted(reads.items()):
        if path in manifest.cargo_exempt:
            continue
        package = packages[package_dir(path, packages) or ""]
        entry = entries.get(package)
        if entry is None or path not in entry.readers:
            lines = ", ".join(str(line) for line in sorted({read.line for read in found}))
            problems.append(
                f"{path}:{lines}: reads repository files outside package {package}; list it under "
                f"[[cargo]] package = \"{package}\" in {MANIFEST} with the paths it reads, so "
                "changing them runs its tests")
            continue
        for read in found:
            if read.target is not None:
                problems.extend(f"{path}:{read.line}: {problem}, which no input of cargo {package} matches"
                                for problem in uncovered(read, entry.inputs, files))
    for entry in manifest.cargo:
        for directory in entry.dependencies_of:
            if not any(package.startswith(directory) for package in packages):
                problems.append(f"{MANIFEST}: cargo {entry.package}: dependencies_of {directory} holds no tracked package")
    return problems


class Tree:
    """The tracked files, sorted, to list what lies under a directory."""

    def __init__(self, tracked: list[str]):
        self.files = sorted(tracked)

    def under(self, directory: str) -> list[str]:
        start = bisect.bisect_left(self.files, directory + "/")
        end = bisect.bisect_left(self.files, directory + "0")  # "0" sorts right after "/"
        return self.files[start:end]

    def at(self, path: str) -> list[str]:
        """`path` if it is a file, else the files under it."""
        index = bisect.bisect_left(self.files, path)
        return [path] if index < len(self.files) and self.files[index] == path else self.under(path)

    def entries(self, directory: str) -> list[str]:
        """The names directly in `directory` that hold files."""
        return sorted({path[len(directory) + 1:].split("/")[0] for path in self.under(directory)
                       if "/" in path[len(directory) + 1:]})


def uncovered(read: CrossRead, inputs: tuple[str, ...], tree: Tree) -> list[str]:
    """What of `read` no input matches. A file must match an input, a directory must have a file
    that does, and a directory the code lists must have such a file in every entry, at each name
    the code joins onto its entries that the entry has. What the tree doesn't hold yet isn't
    checked: a commit adding it runs this audit again."""
    assert read.target is not None

    def covered(paths: list[str]) -> bool:
        return not paths or any(matches(path, pattern) for path in paths for pattern in inputs)

    if not read.runtime:
        return [] if covered([read.target]) else [f"includes {read.target}"]
    if read.each is None:
        return [] if covered(tree.at(read.target)) else [f"reads {read.target}"]
    problems = []
    for entry in tree.entries(read.target):
        prefix = f"{read.target}/{entry}"
        for name in read.each or ("",):
            path = f"{prefix}/{name}" if name else prefix
            if not covered(tree.at(path)):
                problems.append(f"lists {read.target} and reads {path}")
    return problems


def tracked_files(root: Path) -> list[str]:
    """The files the tree being checked holds: tracked, less those it deletes,
    plus those it adds.

    Under t a commit the server has not seen yet is checked as uncommitted
    changes on the commit it branched from, so a file the change deletes is
    still in git's index while absent from the tree, and a file it adds is
    untracked (but not ignored) while present in it.
    """
    def listed(*options: str) -> list[str]:
        output = subprocess.run(["git", "ls-files", "-z", *options], cwd=root, check=True,
                                stdout=subprocess.PIPE).stdout.decode("utf-8")
        return [path for path in output.split("\0") if path]
    deleted = set(listed("--deleted"))
    added = listed("--others", "--exclude-standard")
    return [path for path in listed() if path not in deleted] + added


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    run_parser = commands.add_parser("run", help="run the named suites")
    run_parser.add_argument("suites", nargs="*")
    run_parser.add_argument("--all", action="store_true", help="run every declared suite")
    run_parser.add_argument("--group", action="append", default=[], metavar="NAME",
                            help="run every suite of this group (repeatable)")
    run_parser.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    commands.add_parser("list", help="print every declared suite")
    commands.add_parser("check", help="verify every test-shaped file is accounted for")
    args = parser.parse_args(argv)
    if args.command == "list":
        print("\n".join(load(ROOT).suites()))
        return 0
    if args.command == "check":
        problems = audit(ROOT, tracked_files(ROOT))
        # Findings on stdout, everything else on stderr (tools/policy/findings.py).
        for problem in problems:
            print(problem)
        if not problems:
            manifest = load(ROOT)
            print(f"suite manifest OK: {len(manifest.suites())} path-selected, "
                  f"{len(manifest.covered)} covered, {len(manifest.unrequired)} unrequired; "
                  f"{len(manifest.cargo)} Cargo packages with cross-package inputs", file=sys.stderr)
        return 1 if problems else 0
    if sum(map(bool, (args.all, args.suites, args.group))) > 1:
        parser.error("run takes either suite paths, --group or --all")
    manifest = load(ROOT)
    if args.group:
        groups = {group.name: group for group in manifest.groups}
        unknown = [name for name in args.group if name not in groups]
        if unknown:
            parser.error(f"no such group in {MANIFEST}: {', '.join(unknown)}")
        selected = [suite for name in args.group for suite in groups[name].suites]
    else:
        # An empty selection is an error, never "everything": the workflow passes
        # the selector's list, and an empty list there means selection broke.
        selected = manifest.suites() if args.all else args.suites
    return run(ROOT, selected, args.jobs)


if __name__ == "__main__":
    sys.exit(main())
