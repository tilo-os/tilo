#!/usr/bin/env python3
"""Programs outside Tilo, built on the published crates alone, build, run and print what they should.

Each proof is a guide that shows a whole program: its files are the code blocks marked
`<!-- file: PATH -->` (blocks naming the same file are joined in order), and what it prints is
the block marked `<!-- output -->`. For each one this

1. packages the crates the proof names and the crates of the set they depend on
   (`publish_check.py --crates`, a `cargo publish --dry-run` of them), at the version the
   guide's Cargo.toml asks for;
2. builds a Cargo local registry of those packages and the crates.io packages they resolve to
   (`local_registry.py`);
3. writes the guide's files into a new project outside the repository, with no `[patch]` and no
   paths: it depends on the crates by version, as anyone's project does;
4. builds and runs it offline, with crates.io replaced by the registry, and fails unless it
   prints exactly the guide's output.

So the guide cannot show code that does not build on what is published, nor output the code
does not print, and a crate whose package does not build on its own (a build script reading
the repository, a file the package leaves out) fails here before a release does.

The `tilolang` proof does the same for the command: it installs the packaged `tilolang-cli`
with `cargo install` into a directory outside the repository, and there, with no checkout
anywhere around it, starts an app with `tilolang new`, checks it and builds it, then checks and
builds a tilolang package copied out of the repository. A command that reads a file of the
checkout it was compiled in fails here, as it would on a machine that never had one.

    python3 -I tools/sdk/published_crates_proof.py            # every proof
    python3 -I tools/sdk/published_crates_proof.py embedding  # one
    python3 -I tools/sdk/published_crates_proof.py --work DIR # keep the projects and registry in DIR

Resolving the registry asks crates.io for the packages Cargo's caches lack; building the
programs is offline.

A proof that opens a window cannot open one on a build machine with no display. Such a guide's
program takes a flag (`args` below) and drives the same window through the toolkit's own headless
host instead: the same widgets, layout and input dispatch, with no window and no GPU. It prints
what it laid out, and the guide shows that output. For `tilo-ui` the flag is `--headless`; the
guide explains what it does where a reader meets it.
"""

from __future__ import annotations

import argparse
import dataclasses
import difflib
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib

REPO = pathlib.Path(__file__).resolve().parents[2]
SDK = REPO / "tools/sdk"


@dataclasses.dataclass(frozen=True)
class Proof:
    # The guide that shows the program and what it prints, from the repository's root.
    guide: str
    # The crates of the set the program depends on: they and what they depend on are packaged.
    crates: tuple[str, ...]
    # Arguments the program is run with, after `--`: empty when it runs with none.
    args: tuple[str, ...] = ()


PROOFS = {
    # Compile a tilolang package, run an action on its resource, read it back and build its view.
    "embedding": Proof(guide="docs/tilolang/embedding.md", crates=("tilolang", "tilolang-runtime")),
    # Lay out a window's shared components and press its button in the toolkit's headless host.
    "tilo-ui": Proof(guide="docs/tilolang/tilo-ui.md", crates=("tilo-ui",), args=("--headless",)),
    # The window the toolkit's README shows, as crates.io shows it.
    "tilo-ui-readme": Proof(guide="crates/ui/ui-core/README.md", crates=("tilo-ui",), args=("--headless",)),
}

# The `tilolang` command, installed from its package: what it is installed from, and the
# tilolang package from this repository it checks and builds, copied out first.
COMMAND = "tilolang"
COMMAND_CRATES = ("tilolang-cli",)
COMMAND_PACKAGE = "apps/tl/counter"

FILE = re.compile(r"^<!-- file: (\S+) -->$")
OUTPUT = "<!-- output -->"
FENCE = re.compile(r"^(`{3,}|~{3,})")


@dataclasses.dataclass
class Program:
    files: dict[str, str]
    output: str


def program(guide: pathlib.Path) -> Program:
    """The files and output GUIDE shows: each marked code block, the marker on the line before
    its fence."""
    lines = guide.read_text().splitlines()
    files: dict[str, str] = {}
    output: str | None = None
    at = 0
    while at < len(lines):
        line = lines[at]
        file = FILE.match(line)
        if not file and line != OUTPUT:
            at += 1
            continue
        fence = FENCE.match(lines[at + 1]) if at + 1 < len(lines) else None
        if not fence:
            raise SystemExit(f"{guide}:{at + 1}: `{line}` is not followed by a code block")
        end = next((i for i in range(at + 2, len(lines))
                    if lines[i].startswith(fence.group(1)) and not lines[i].strip("`~")), None)
        if end is None:
            raise SystemExit(f"{guide}:{at + 2}: the code block does not end")
        text = "\n".join(lines[at + 2:end]) + "\n"
        if file:
            files[file.group(1)] = files.get(file.group(1), "") + text
        elif output is not None:
            raise SystemExit(f"{guide}:{at + 1}: a second output block")
        else:
            output = text
        at = end + 1
    if "Cargo.toml" not in files:
        raise SystemExit(f"{guide} shows no Cargo.toml")
    if output is None:
        raise SystemExit(f"{guide} shows no output")
    return Program(files, output)


def asked_version(manifest: str, crate: str) -> str:
    """The version to package CRATE at for MANIFEST's requirement on it: the lowest release it
    takes ("0.1" takes 0.1.0)."""
    parsed = tomllib.loads(manifest)
    if "patch" in parsed or "replace" in parsed:
        raise SystemExit("the guide's Cargo.toml patches crates: it must build on what is published")
    deps = parsed.get("dependencies", {})
    if crate not in deps:
        raise SystemExit(f"the guide's Cargo.toml does not depend on {crate}")
    spec = deps[crate]
    requirement = spec if isinstance(spec, str) else spec.get("version")
    if not isinstance(spec, str) and ("path" in spec or "git" in spec):
        raise SystemExit(f"the guide's Cargo.toml names {crate} by {('path' if 'path' in spec else 'git')}")
    match = re.fullmatch(r"[\^=~]?(\d+)(?:\.(\d+))?(?:\.(\d+))?", requirement or "")
    if not match:
        raise SystemExit(f"the guide asks for {crate} {requirement!r}, not a plain version")
    return ".".join(part or "0" for part in match.groups())


def run(command: list[str], **kwargs) -> subprocess.CompletedProcess:
    print("$ " + " ".join(command), flush=True)
    return subprocess.run(command, check=False, **kwargs)


def prove(name: str, proof: Proof, shown: Program, work: pathlib.Path,
          registry: pathlib.Path) -> bool:
    project = work / name
    if project.exists():
        shutil.rmtree(project)
    for path, text in shown.files.items():
        target = (project / path).resolve()
        if not target.is_relative_to(project.resolve()):
            raise SystemExit(f"{proof.guide}: {path} is outside the project")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
    # Its own target directory, whatever the job builds the workspace into.
    env = {key: value for key, value in os.environ.items()
           if key not in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR")}
    env["CARGO_NET_OFFLINE"] = "true"
    done = run(
        ["cargo", "run", "--quiet", "--offline",
         "--config", 'source.crates-io.replace-with="tilo-published"',
         "--config", f"source.tilo-published.local-registry={str(registry)!r}",
         *(["--", *proof.args] if proof.args else [])],
        cwd=project, env=env, stdout=subprocess.PIPE, text=True,
    )
    print(done.stdout, end="", flush=True)
    if done.returncode != 0:
        print(f"error: {name}: the program failed (exit {done.returncode})", file=sys.stderr)
        return False
    if done.stdout != shown.output:
        diff = difflib.unified_diff(shown.output.splitlines(keepends=True),
                                    done.stdout.splitlines(keepends=True),
                                    f"{proof.guide} (shown)", "printed")
        print(f"error: {name}: the program does not print what {proof.guide} shows:\n"
              + "".join(diff), file=sys.stderr)
        return False
    print(f"{name}: builds on the published crates and prints what {proof.guide} shows")
    return True


def prove_command(work: pathlib.Path, registry: pathlib.Path, version: str) -> bool:
    """Install the packaged `tilolang` outside the repository, and with it start, check and
    build an app and check and build a package, as someone with no checkout does."""
    root = work / COMMAND
    if root.exists():
        shutil.rmtree(root)
    apps = root / "apps"
    (apps / ".cargo").mkdir(parents=True)
    # The registry stands in for crates.io, for the install and for every app under `apps`.
    (apps / ".cargo/config.toml").write_text(
        '[source.crates-io]\nreplace-with = "tilo-published"\n\n'
        f'[source.tilo-published]\nlocal-registry = {str(registry)!r}\n'
        '[net]\noffline = true\n')
    home = root / "home"
    env = {key: value for key, value in os.environ.items()
           if key not in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "TILO_REPO",
                          "TILO_UI_PATH", "TILO_NESTED_SESSION_BUILD")}
    env.update({"CARGO_NET_OFFLINE": "true",
                "XDG_CONFIG_HOME": str(home / "config"), "XDG_DATA_HOME": str(home / "data"),
                "XDG_STATE_HOME": str(home / "state"), "XDG_CACHE_HOME": str(home / "cache")})
    bin_dir = root / "install/bin"
    env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"

    def step(what: str, command: list[str], cwd: pathlib.Path) -> bool:
        if run(command, cwd=cwd, env=env).returncode == 0:
            return True
        print(f"error: {COMMAND}: {what} failed", file=sys.stderr)
        return False

    package = apps / pathlib.Path(COMMAND_PACKAGE).name
    shutil.copytree(REPO / COMMAND_PACKAGE, package)
    ok = (step("cargo install tilolang-cli",
               # `cargo install` reads no configuration of the directory it runs in.
               ["cargo", "install", "tilolang-cli", "--version", f"={version}",
                "--root", str(root / "install"),
                "--config", 'source.crates-io.replace-with="tilo-published"',
                "--config", f"source.tilo-published.local-registry={str(registry)!r}"], apps)
          and step("tilolang --help", ["tilolang", "--help"], apps)
          and step("tilolang new", ["tilolang", "new", "demo"], apps)
          and step("tilolang check (the new app)", ["tilolang", "check"], apps / "demo")
          and step("tilolang build (the new app)", ["tilolang", "build"], apps / "demo")
          and step("tilolang check (a package)", ["tilolang", "check", str(package)], apps)
          and step("tilolang build (a package)",
                   ["tilolang", "build", str(package), "--out", str(root / "out")], apps))
    if ok:
        print(f"{COMMAND}: the installed command starts, checks and builds an app and checks and "
              "builds a package, with no checkout")
    return ok


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("proofs", nargs="*", metavar="PROOF",
                        help=f"the proofs to run (default: all of {', '.join(PROOFS)})")
    parser.add_argument("--work", type=pathlib.Path,
                        help="keep the packages, registry and projects here (default: a "
                        "directory removed afterwards); outside the repository")
    args = parser.parse_args()
    every = [*PROOFS, COMMAND]
    unknown = [name for name in args.proofs if name not in every]
    if unknown:
        parser.error(f"no proof {', '.join(unknown)}; the proofs are {', '.join(every)}")
    command = not args.proofs or COMMAND in args.proofs
    chosen = {name: PROOFS[name] for name in (args.proofs or PROOFS) if name in PROOFS}
    # Every guide's version, chosen or not: the command is packaged at the release's too.
    shown = {name: program(REPO / proof.guide) for name, proof in PROOFS.items()}
    versions = {asked_version(shown[name].files["Cargo.toml"], crate)
                for name, proof in PROOFS.items() for crate in proof.crates}
    if len(versions) != 1:
        # One release carries every crate at one version (publish_check.py).
        raise SystemExit(f"the guides ask for the crates at more than one version: {sorted(versions)}")
    version = versions.pop()
    crates = sorted({crate for proof in chosen.values() for crate in proof.crates}
                    | (set(COMMAND_CRATES) if command else set()))

    work = args.work.resolve() if args.work else pathlib.Path(
        tempfile.mkdtemp(prefix="tilo-published-proof-", dir=os.environ.get("TMPDIR")))
    if work.is_relative_to(REPO):
        # Cargo would take a project under the repository for a member of its workspace.
        raise SystemExit(f"the work directory {work} must be outside the repository")
    try:
        packages = work / "packages"
        shutil.rmtree(packages, ignore_errors=True)
        if run([sys.executable, "-I", str(SDK / "publish_check.py"), "--crates", ",".join(crates),
                "--version", version, "--crates-out", str(packages)]).returncode != 0:
            return 1
        registry = work / "registry"
        if run([sys.executable, "-I", str(SDK / "local_registry.py"), str(registry),
                "--packages", str(packages)]).returncode != 0:
            return 1
        failed = [name for name, proof in chosen.items()
                  if not prove(name, proof, shown[name], work, registry)]
        if command and not prove_command(work, registry, version):
            failed.append(COMMAND)
    finally:
        if not args.work:
            shutil.rmtree(work, ignore_errors=True)
    if failed:
        print(f"error: failed: {', '.join(failed)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
