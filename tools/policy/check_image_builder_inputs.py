#!/usr/bin/env python3
"""Keep the image builders' source copies complete, and narrow.

The Rust compiles of the OS image and of the dev image's components
(os/image/compile.sh, outside any image build, keyed by what os/Containerfile's
`builder-sources` stage copies) and the compiled tree's packing in the
`apps-payload` stage read only what that stage copies, never the whole build
context: with `COPY . /build`, editing a unit, an /etc file or a test script
under os/ re-ran the compile and relinked every binary although none of them
depends on it.

A narrow copy fails the other way when it misses an input, so each stage must
copy, to the same path under /build:

  * the workspace manifests, Cargo.lock, rust-toolchain.toml, .cargo/ and every
    workspace member (and vendor/, which the patches resolve to);
  * every repository file a workspace crate include!s from outside the
    workspace (tools/ci/suites.py finds them), except includes that only a
    test compiles;
  * every repository file its RUN steps name, as /build/<path> or relative to
    the stage's WORKDIR, /build, and every one the scripts that compile from the
    same copy name (os/image/compile.sh, run at the repository root).

A compile stage may take these from a scratch stage whole
(`COPY --from=<stage> /build/ /build/`, one layer instead of one per COPY);
that stage's copies then count as its own.

And neither stage may copy the whole context back in.

Every finding names file:line, why it matters, and the fix.
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import re
import shlex
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
RERUN = "python3 tools/policy/check_image_builder_inputs.py   (or tools/policy/run.sh)"
# (Containerfile, compile stage, scripts run against the same copy) this policy covers.
STAGES = (("os/Containerfile", "apps-payload", ("os/image/compile.sh",)),)
WORKDIR = "/build"
# Always read by a workspace build.
WORKSPACE_FILES = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml")
# Under /build but not repository paths: cache mounts and build output.
NOT_SOURCES = ("target", ".cargo-home", ".sccache")
WHY_MISSING = ("the stage builds from a narrow copy of the repository; a file it reads but does not copy "
               "fails the build, or worse, silently takes a fallback, and only after the cold compile")
WHY_WHOLE = ("copying the whole context makes every change anywhere in the repository, a unit, an /etc "
             "file, a test script, re-run the compile and relink every binary")
REPO_PATH = re.compile(r"(?<![\w./-])(?:/build/)?((?:os|tools|dist|docs|keys|scripts)/[A-Za-z0-9_.+/-]*[A-Za-z0-9_+-])")
BUILD_PATH = re.compile(r"(?<![\w.-])/build/([A-Za-z0-9_.+-][A-Za-z0-9_.+/-]*)")


@dataclass(frozen=True)
class Finding:
    where: str
    what: str
    why: str
    fix: str

    def headline(self) -> str:
        """The finding, one line: what main prints on stdout."""
        return f"{self.where}: {self.what}"

    def detail(self) -> str:
        """Why it matters and the fix: what main prints on stderr after the headline."""
        return f"  why: {self.why}.\n  fix: {self.fix}\n  re-run: {RERUN}"

    def __str__(self) -> str:
        return f"{self.headline()}\n{self.detail()}"


@dataclass(frozen=True)
class Copy:
    line: int
    sources: tuple[str, ...]
    destination: str


def load_suites(repo: Path):
    spec = importlib.util.spec_from_file_location("tilo_ci_suites", repo / "tools/ci/suites.py")
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def logical_lines(text: str) -> list[tuple[int, str]]:
    """(first line number, joined text) of backslash-continued lines, comments dropped."""
    result: list[tuple[int, str]] = []
    pending, start = "", 0
    for number, raw in enumerate(text.splitlines(), 1):
        stripped = raw.strip()
        if stripped.startswith("#"):
            continue
        if not pending:
            start = number
        continued = stripped.endswith("\\")
        pending = f"{pending} {stripped[:-1] if continued else stripped}".strip()
        if not continued:
            if pending:
                result.append((start, pending))
            pending = ""
    if pending:
        result.append((start, pending))
    return result


def stage_lines(text: str, stage: str) -> list[tuple[int, str]]:
    """The logical lines of one named stage, FROM excluded."""
    lines: list[tuple[int, str]] = []
    inside = False
    for number, line in logical_lines(text):
        if re.match(r"FROM\s", line, re.I):
            inside = bool(re.search(rf"\sAS\s+{re.escape(stage)}\s*$", line, re.I))
            continue
        if inside:
            lines.append((number, line))
    return lines


def context_copies(lines: list[tuple[int, str]]) -> list[Copy]:
    """COPY instructions that read the build context (no --from)."""
    copies: list[Copy] = []
    for number, line in lines:
        if not re.match(r"COPY\s", line, re.I):
            continue
        words = [w for w in shlex.split(line)[1:]]
        if any(w.startswith("--from") for w in words):
            continue
        words = [w for w in words if not w.startswith("--")]
        if len(words) < 2:
            continue
        copies.append(Copy(number, tuple(w.rstrip("/") or "." for w in words[:-1]), words[-1]))
    return copies


def source_stage_copies(text: str, lines: list[tuple[int, str]]) -> list[Copy]:
    """Context copies of a scratch stage the compile stage takes whole.

    `COPY --from=<stage> /build/ /build/` brings in everything that stage
    copied to /build, as one layer; its copies count as the compile stage's.
    """
    copies: list[Copy] = []
    for _, line in lines:
        match = re.fullmatch(rf"COPY\s+--from=(\S+)\s+{WORKDIR}/?\s+{WORKDIR}/?", line.strip())
        if match:
            copies.extend(context_copies(stage_lines(text, match.group(1))))
    return copies


def image_path(copy: Copy, source: str) -> str:
    """Where one source of a COPY lands."""
    if len(copy.sources) > 1 or copy.destination.endswith("/"):
        return copy.destination.rstrip("/") + "/" + source.rsplit("/", 1)[-1]
    return copy.destination


def covered(path: str, copies: list[Copy]) -> bool:
    """Is repository `path` copied to /build/<path>?"""
    for copy in copies:
        for source in copy.sources:
            if source == ".":
                return True
            if path == source or path.startswith(source + "/"):
                if image_path(copy, source) + path[len(source):] == f"{WORKDIR}/{path}":
                    return True
    return False


def workspace_roots(repo: Path) -> list[str]:
    workspace = tomllib.loads((repo / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
    roots = {member.split("/", 1)[0] for member in workspace.get("members", [])}
    return sorted(roots | {"vendor"})


def test_only(text: str, offset: int) -> bool:
    """Is the include at `offset` inside a #[cfg(test)] module?"""
    before = text[:offset]
    marker = before.rfind("#[cfg(test)]")
    return marker != -1 and re.match(r"#\[cfg\(test\)\]\s*(pub(\([^)]*\))?\s+)?mod\s", before[marker:]) is not None


def compile_includes(repo: Path, tracked: list[str], roots: list[str]) -> dict[str, list[str]]:
    """Repository files outside the workspace roots that release code include!s, and who does."""
    suites = load_suites(repo)
    packages = suites.cargo_packages(repo, tracked)
    reads = suites.cross_package_reads(repo, tracked, packages)
    found: dict[str, list[str]] = {}
    for reader, entries in sorted(reads.items()):
        if "/tests/" in f"/{reader}" or "/benches/" in f"/{reader}" or reader.endswith("/tests.rs"):
            continue
        text = (repo / reader).read_text(encoding="utf-8", errors="replace")
        line_offsets = [0]
        for line in text.splitlines(keepends=True):
            line_offsets.append(line_offsets[-1] + len(line))
        for read in entries:
            target = read.target
            if target is None or read.runtime or target.split("/", 1)[0] in roots:
                continue
            if test_only(text, line_offsets[read.line - 1]):
                continue
            found.setdefault(target, []).append(f"{reader}:{read.line}")
    return found


def run_references(lines: list[tuple[int, str]]) -> list[tuple[int, str]]:
    """Repository paths a stage's RUN steps name."""
    refs: list[tuple[int, str]] = []
    for number, line in lines:
        if not re.match(r"RUN\s", line, re.I):
            continue
        # A context bind mount hands the RUN its file directly; it is not read
        # from the stage's copy under /build.
        line = re.sub(r"--mount=type=bind,(?![^ ]*\bfrom=)[^ ]*", "", line)
        for match in BUILD_PATH.finditer(line):
            path = match.group(1).rstrip("/")
            if path.split("/", 1)[0] in NOT_SOURCES or "$" in path or "*" in path:
                continue
            refs.append((number, path))
        for match in REPO_PATH.finditer(line):
            path = match.group(1)
            if "*" in path:
                continue
            refs.append((number, path))
    return refs


def script_references(repo: Path, script: str) -> list[tuple[int, str]]:
    """Repository paths a script run at the repository root names, comments left out."""
    path = repo / script
    if not path.exists():
        return []
    refs: list[tuple[int, str]] = []
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        code = line.split("#", 1)[0] if line.lstrip().startswith("#") else line
        for match in REPO_PATH.finditer(code):
            if "*" not in match.group(1):
                refs.append((number, match.group(1)))
    return refs


SCRIPT_RELATIVE = re.compile(r"dirname[^)]*\)\"?/((?:\.\./)+[A-Za-z0-9_.+/-]*[A-Za-z0-9_+-])")


def copied_script_references(repo: Path, copies: list[Copy], tracked: list[str],
                             roots: list[str]) -> list[tuple[str, str]]:
    """Repository files the scripts copied for the stage (outside the workspace) read beside
    themselves, as `$(dirname "${BASH_SOURCE[0]}")/../../<path>`: the stage runs them from its
    copy, so what they read must be in it too."""
    refs: list[tuple[str, str]] = []
    for script in tracked:
        if (not script.endswith((".sh", ".py")) or script.split("/", 1)[0] in roots
                or not covered(script, copies)):
            continue
        base = Path(script).parent
        for number, line in enumerate((repo / script).read_text(encoding="utf-8", errors="replace").splitlines(), 1):
            if line.lstrip().startswith("#"):
                continue
            for match in SCRIPT_RELATIVE.finditer(line):
                target = Path(os.path.normpath(base / match.group(1)))
                if not str(target).startswith(".."):
                    refs.append((f"{script}:{number}", str(target)))
    return refs


def audit_stage(repo: Path, containerfile: str, stage: str, scripts: tuple[str, ...], tracked: list[str],
                includes: dict[str, list[str]], roots: list[str]) -> list[Finding]:
    text = (repo / containerfile).read_text(encoding="utf-8")
    lines = stage_lines(text, stage)
    if not lines:
        return [Finding(containerfile, f"has no stage named {stage}",
                        "this policy guards that stage's narrow source copy",
                        f"update STAGES in {Path(__file__).name} if the stage was renamed")]
    copies = context_copies(lines) + source_stage_copies(text, lines)
    findings: list[Finding] = []
    for copy in copies:
        if "." in copy.sources:
            findings.append(Finding(f"{containerfile}:{copy.line}", f"stage {stage} copies the whole build context",
                                    WHY_WHOLE, "copy only what the stage reads (see this policy's docstring)"))
    tracked_set = set(tracked)
    required: list[tuple[str, str]] = []
    for path in WORKSPACE_FILES:
        required.append((path, "a workspace build reads it"))
    for root in roots:
        required.append((root, "it holds workspace members or the vendored patches"))
    for target, readers in sorted(includes.items()):
        required.append((target, f"{readers[0]} include!s it"))
    refs = [(f"{containerfile}:{number}", path) for number, path in run_references(lines)]
    refs += [(f"{script}:{number}", path) for script in scripts for number, path in script_references(repo, script)]
    refs += copied_script_references(repo, copies, tracked, roots)
    for where, path in refs:
        if path in tracked_set or any(t.startswith(path + "/") for t in tracked):
            required.append((path, f"{where} reads it"))
    seen: set[str] = set()
    for path, reason in required:
        if path in seen:
            continue
        seen.add(path)
        if not covered(path, copies):
            findings.append(Finding(containerfile, f"stage {stage} does not copy {path} to {WORKDIR}/{path} ({reason})",
                                    WHY_MISSING, f"add `COPY {path} {WORKDIR}/{path}` to the stage"))
    return findings


def audit(repo: Path = REPO, tracked: list[str] | None = None) -> list[Finding]:
    if tracked is None:
        tracked = load_suites(repo).tracked_files(repo)
    roots = workspace_roots(repo)
    includes = compile_includes(repo, tracked, roots)
    findings: list[Finding] = []
    for containerfile, stage, scripts in STAGES:
        if (repo / containerfile).exists():
            findings.extend(audit_stage(repo, containerfile, stage, scripts, tracked, includes, roots))
    return findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", type=Path, default=REPO)
    args = parser.parse_args(argv)
    findings = audit(args.repo)
    for finding in findings:
        sys.stderr.flush()
        print(finding.headline(), flush=True)
        print(finding.detail(), file=sys.stderr)
    if findings:
        print(f"\n{len(findings)} image builder input problem(s)", file=sys.stderr)
        return 1
    print("image builder inputs OK", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
