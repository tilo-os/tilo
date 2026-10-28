#!/usr/bin/env python3
"""Run t.toml's checks on one machine with podman, without a t server: the named checks and the
checks they need, each in its image as t runs it, in dependency order. t.toml stays the only
definition; this reads it (tools/ci/t_matrix.py expands its matrices) and runs what it says.

  run-checks.py [--target DIR] [--work DIR] [--jobs N] [--force] [--dry-run] CHECK...

What a check gets, as on t (t's README, "Where checks run" and "Micro-VM steps"):
  * its `image` (the project's when it names none; `{ check, path }` is the OCI layout or
    archive that check wrote below /target), with the tree at /src (read-only, copy-on-write
    with `writable_sources`, writable for a check with outputs in the tree, which the checks
    after it see there) and its .git with `git = true`;
  * one /target shared by every check of the run, where the outputs of those it needs are;
  * t.toml's [env] and its own `env`, HOME, TMPDIR and CARGO_TARGET_DIR (a directory of its own
    below /target, kept between runs: cargo builds of checks run at once must not share one), and
    T_COMMIT with `git = true`, secrets or identity tokens; T_REF from --ref;
  * `isolation = "vm"`: root with podman of its own and /dev/kvm (here a privileged container
    with its own /var/lib/containers, /tmp and /var/tmp, but the machine's kernel: no loop
    devices and only the filesystems it has, so os-dev-disk-<arch> does not run here);
  * its `setup` before its `run`, its `requirement` as the container's cores and memory, and
    `timeout_secs`.

A check passed before with the same key (its command, image, environment, the files its `inputs`
name, or the whole tree when it names none or builds cargo packages, and the keys of what it
needs) is not run again unless --force. A check's `secrets` come from this environment, named
with --secret; of t's identity tokens only registry.tilo.so's has a stand-in, REGISTRY_TOKEN (a
Forgejo token that may push to this repository, lib.sh registry_login). A check that needs
another (signing) runs on t, or its outputs are made elsewhere and it is named with --given.

The machine needs python3, git and a local podman (a remote one pulls nothing but registry
images, and the checks' own images are OCI layouts on disk).
"""
from __future__ import annotations

import argparse
import concurrent.futures
import functools
import hashlib
import json
import os
import re
from pathlib import Path
import shlex
import subprocess
import sys
import threading
import time
import tomllib

sys.path.insert(0, str(Path(__file__).resolve().parent))
import t_matrix  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
# t runs a check's `run` with bash and pipefail where the image has bash (t's exec.rs).
SHELL = 'if command -v bash >/dev/null 2>&1; then exec bash -eo pipefail -c "$0"; else set -e; eval "$0"; fi'
STORAGE_CONF = """[storage]
driver = "overlay"
runroot = "/run/containers/storage"
graphroot = "/var/lib/containers/storage"
[storage.options.overlay]
mountopt = "nodev"
"""
PATH = "/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"


def die(message: str) -> None:
    print(f"run-checks: {message}", file=sys.stderr)
    raise SystemExit(1)


def say(message: str) -> None:
    print(f"run-checks: {time.strftime('%H:%M:%S')} {message}", flush=True)


def image_of(check: dict, project: dict) -> str | dict | None:
    image = check.get("image", project.get("image"))
    if image is False:
        die(f"{check['name']} runs on the node's own tools (image = false); run it there")
    return image


def needs_of(check: dict, project: dict) -> list[str]:
    needs = list(check.get("needs", []))
    image = image_of(check, project)
    if isinstance(image, dict) and image["check"] not in needs:
        needs.append(image["check"])
    return needs


def closure(names: list[str], checks: dict[str, dict], project: dict) -> list[str]:
    order: list[str] = []
    visiting: set[str] = set()

    def visit(name: str) -> None:
        if name in order:
            return
        if name not in checks:
            die(f"t.toml has no check {name}")
        if name in visiting:
            die(f"{name} needs itself")
        visiting.add(name)
        for need in needs_of(checks[name], project):
            visit(need)
        visiting.discard(name)
        order.append(name)

    for name in names:
        visit(name)
    return order


@functools.cache
def pattern_re(pattern: str) -> re.Pattern[str]:
    """A t input glob as a regex: `**` spans directories, `*` and `?` stay within one, and a
    plain path names a file or everything below a directory."""
    if not any(c in pattern for c in "*?["):
        return re.compile(re.escape(pattern.rstrip("/")) + "(?:/.*)?")
    out, i = "", 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out, i = out + "(?:.*/)?", i + 3
        elif pattern.startswith("**", i):
            out, i = out + ".*", i + 2
        elif pattern[i] == "*":
            out, i = out + "[^/]*", i + 1
        elif pattern[i] == "?":
            out, i = out + "[^/]", i + 1
        elif pattern[i] == "[":
            j = pattern.index("]", i)
            out, i = out + pattern[i:j + 1], j + 1
        else:
            out, i = out + re.escape(pattern[i]), i + 1
    return re.compile(out)


def matches(path: str, pattern: str) -> bool:
    return pattern_re(pattern).fullmatch(path) is not None


class Tree:
    """The files of the checkout, as git has them at HEAD, with their blob ids."""

    def __init__(self, root: Path):
        self.root = root
        out = subprocess.run(["git", "-C", str(root), "ls-tree", "-r", "--full-tree", "HEAD"],
                             check=True, capture_output=True, text=True).stdout
        self.blobs = {}
        for line in out.splitlines():
            meta, path = line.split("\t", 1)
            self.blobs[path] = meta.split()[2]
        self.commit = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"], check=True,
                                     capture_output=True, text=True).stdout.strip()
        self.tree = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD^{tree}"], check=True,
                                   capture_output=True, text=True).stdout.strip()
        dirty = subprocess.run(["git", "-C", str(root), "status", "--porcelain", "--untracked-files=no"],
                               check=True, capture_output=True, text=True).stdout
        if dirty.strip():
            die(f"{root} has uncommitted changes; checks run on what git has at HEAD")

    def digest(self, patterns: list[str]) -> str:
        h = hashlib.sha256()
        for path in sorted(self.blobs):
            if any(matches(path, p) for p in patterns):
                h.update(f"{path}\0{self.blobs[path]}\n".encode())
        return h.hexdigest()


class Runner:
    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.project = t_matrix.load(ROOT)
        self.checks = {c["name"]: c for c in t_matrix.checks(ROOT)}
        self.tree = Tree(ROOT)
        self.target = Path(args.target).resolve()
        self.work = Path(args.work).resolve()
        self.state = self.target / ".run-checks"
        self.keys: dict[str, str] = {}
        self.pulling: dict[str, threading.Lock] = {}
        self.run_id = f"{os.getpid()}-{int(time.time())}"

    def key(self, name: str) -> str:
        if name in self.keys:
            return self.keys[name]
        check = self.checks[name]
        inputs = check.get("inputs")
        if inputs is None or check.get("cargo_packages") is not None:
            files = self.tree.tree
        else:
            files = self.tree.digest(list(self.project.get("inputs", [])) + list(inputs)
                                     + [s for s in [check.get("setup", {}).get("inputs")] if s for s in s])
        material = {
            "check": {k: v for k, v in check.items() if k not in ("inputs", "package_inputs")},
            "env": self.project.get("env", {}),
            "image": image_of(check, self.project),
            "files": files,
            "commit": self.tree.commit if check.get("git") else None,
            "needs": {n: self.key(n) for n in needs_of(check, self.project)},
        }
        self.keys[name] = hashlib.sha256(json.dumps(material, sort_keys=True).encode()).hexdigest()
        return self.keys[name]

    def passed(self, name: str) -> bool:
        stamp = self.state / f"{name}.passed"
        return stamp.is_file() and stamp.read_text().strip() == self.key(name)

    def podman(self, name: str, *argv: str, **kw) -> subprocess.CompletedProcess:
        return subprocess.run(["podman", *argv], **kw)

    def image(self, name: str) -> str:
        image = image_of(self.checks[name], self.project)
        # One pull per image, however many checks start in it at once.
        with self.pulling.setdefault(json.dumps(image, sort_keys=True), threading.Lock()):
            return self.pull(name, image)

    def pull(self, name: str, image: str | dict) -> str:
        if isinstance(image, str):
            if self.podman(name, "image", "exists", image).returncode != 0:
                self.podman(name, "pull", "-q", image, check=True, stdout=subprocess.DEVNULL)
            return image
        built = self.target / image["path"].removeprefix("/target/")
        transport = "oci" if (built / "index.json").is_file() else "oci-archive" if built.is_file() else None
        if transport is None:
            die(f"{name} runs in {image['check']}'s image, but {built} holds none")
        out = self.podman(name, "pull", "-q", f"{transport}:{built}", check=True, capture_output=True, text=True)
        return out.stdout.strip().splitlines()[-1]

    def container(self, name: str) -> str:
        return f"run-checks-{self.run_id}-{name.replace('/', '_')}"

    def remove(self, name: str, path: Path, image: str) -> None:
        """Remove what a check's containers left, owned by the user ids they mapped, in its image."""
        if path.exists():
            self.podman(name, "run", "--rm", "--security-opt", "label=disable", "-v", f"{path.parent}:/w",
                        "--entrypoint", "rm", image, "-rf", "--", f"/w/{path.name}", check=True)

    def run(self, name: str) -> None:
        check = self.checks[name]
        # registry.tilo.so takes a Forgejo token where t's identity token is missing (lib.sh
        # registry_login); nothing else stands in for one.
        secrets = list(check.get("secrets", []))
        for audience in check.get("id_token", []):
            if audience != "registry.tilo.so":
                raise RuntimeError(f"{name} needs t's identity token for {audience}; run it on t, or make its "
                                   "outputs elsewhere and pass --given")
            secrets.append("REGISTRY_TOKEN")
        missing = [s for s in secrets if s not in self.args.secret]
        if missing:
            raise RuntimeError(f"{name} needs the secrets {' '.join(missing)}: pass --secret NAME with NAME set here")
        vm = check.get("isolation") == "vm"
        scratch = self.work / name.replace("/", "_")
        image = self.image(name)
        self.remove(name, scratch, image)
        # t gives a check the outputs of the checks it needs, never its own from an earlier run.
        for output in check.get("outputs", []):
            if output.startswith("/target/"):
                static = output.split("*")[0]
                stale = self.target / static.removeprefix("/target/").rstrip("/")
                if stale != self.target:
                    self.remove(name, stale, image)
        (self.target / ".cargo-target").mkdir(exist_ok=True)
        dirs = ["tmp", "var-tmp"] + (["containers"] if vm else [])
        for d in dirs:
            (scratch / d).mkdir(parents=True)
        # Outputs named by a path in the tree are in /src for the checks that need them, as t
        # gives them; such a check writes them into the checkout itself.
        in_tree = any(not o.startswith("/") for o in check.get("outputs", []))
        src = "" if in_tree else ":O" if check.get("writable_sources") else ":ro"
        argv = ["run", "--rm", "--name", self.container(name),
                "--security-opt", "label=disable", "--workdir", "/src",
                "-v", f"{ROOT}:/src{src}", "-v", f"{self.target}:/target",
                "-v", f"{scratch}/tmp:/tmp", "-v", f"{scratch}/var-tmp:/var/tmp"]
        if vm:
            # Root here is root of a user namespace, where the kernel refuses overlay's redirect_dir
            # option, which images' storage.conf may set (the OS assembler's does): podman in the
            # check gets the default store with plain overlay mounts instead.
            (scratch / "storage.conf").write_text(STORAGE_CONF)
            argv += ["--privileged", "-v", f"{scratch}/containers:/var/lib/containers",
                     "-v", f"{scratch}/storage.conf:/run/run-checks/storage.conf:ro",
                     "-e", "CONTAINERS_STORAGE_CONF=/run/run-checks/storage.conf"]
        if "/dev/kvm" in check.get("requires", []):
            argv += ["--device", "/dev/kvm"]
        requirement = check.get("requirement", self.project.get("defaults", {}).get("requirement", {}))
        if not self.args.no_limits and requirement:
            if "cores" in requirement:
                argv += ["--cpus", str(requirement["cores"])]
            if "memory" in requirement:
                argv += ["--memory", requirement["memory"].replace("GiB", "g").replace("MiB", "m")]
        # t gives each check a /target of its own and its cargo builds go there; here they share one, so
        # each check's cargo builds get a directory of their own in it, kept for the next run.
        cargo_target = f"/target/.cargo-target/{name.replace('/', '_')}"
        env = {"PATH": PATH, "HOME": "/root" if vm else "/tmp", "TMPDIR": "/tmp", "CARGO_TARGET_DIR": cargo_target}
        env.update(self.project.get("env", {}))
        if check.get("git") or check.get("secrets") or check.get("id_token"):
            env["T_COMMIT"] = self.tree.commit
        if self.args.ref:
            env["T_REF"] = self.args.ref
        env.update(check.get("env", {}))
        # Podman here runs as root of a user namespace, so it keeps its runtime files in
        # XDG_RUNTIME_DIR as a rootless podman does; a VM step's root podman never reads it.
        runtime = env.get("XDG_RUNTIME_DIR", "")
        if vm and runtime.startswith("/tmp/"):
            (scratch / "tmp" / runtime.removeprefix("/tmp/")).mkdir(parents=True, mode=0o700)
        for k, v in env.items():
            argv += ["-e", f"{k}={v}"]
        # A secret goes by name, from this environment, never on a command line or into the log.
        for secret in secrets:
            argv += ["-e", secret]
        setup = check.get("setup", {}).get("run")
        script = "sh -c \"$T_SHELL\" \"$1\" && sh -c \"$T_SHELL\" \"$2\"" if setup else "exec sh -c \"$T_SHELL\" \"$2\""
        argv += ["-e", f"T_SHELL={SHELL}"]
        # t runs the command whatever the image's entrypoint (the skopeo image's is skopeo).
        argv += ["--entrypoint", "sh", image, "-c", script, "run-checks", setup or "", check["run"]]
        timeout = check.get("timeout_secs")
        log = self.state / f"{name.replace('/', '_')}.log"
        say(f"{name}: started (log {log})")
        started = time.monotonic()
        with log.open("w") as out:
            out.write(f"# podman {shlex.join(argv)}\n")
            out.flush()
            proc = subprocess.Popen(["podman", *argv], stdout=out, stderr=subprocess.STDOUT)
            try:
                code = proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                self.podman(name, "rm", "-f", self.container(name),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                proc.wait()
                code = None
        took = time.monotonic() - started
        if not self.args.keep_work:
            self.remove(name, scratch, image)
        if code != 0:
            why = f"timed out after {timeout} s" if code is None else f"failed ({code})"
            raise RuntimeError(f"{name} {why} after {took:.0f} s; see {log}")
        (self.state / f"{name}.passed").write_text(self.key(name) + "\n")
        say(f"{name}: passed in {took:.0f} s")

    def main(self) -> int:
        order = closure(self.args.checks, self.checks, self.project)
        self.state.mkdir(parents=True, exist_ok=True)
        self.work.mkdir(parents=True, exist_ok=True)
        for name in self.args.given:
            if name not in order:
                die(f"--given {name}: the run does not need it")
            (self.state / f"{name}.passed").write_text(self.key(name) + "\n")
        todo = [n for n in order if (self.args.force and n not in self.args.given) or not self.passed(n)]
        for name in order:
            print(f"{'run ' if name in todo else 'kept'} {name}")
        if self.args.dry_run or not todo:
            return 0
        done = {n for n in order if n not in todo}
        running: dict[concurrent.futures.Future, str] = {}
        failed: list[str] = []
        with concurrent.futures.ThreadPoolExecutor(self.args.jobs) as pool:
            while todo or running:
                ready = [n for n in todo if all(d in done for d in needs_of(self.checks[n], self.project))]
                while ready and len(running) < self.args.jobs and not failed:
                    name = ready.pop(0)
                    todo.remove(name)
                    running[pool.submit(self.run, name)] = name
                if not running:
                    break
                finished, _ = concurrent.futures.wait(running, return_when=concurrent.futures.FIRST_COMPLETED)
                for future in finished:
                    name = running.pop(future)
                    try:
                        future.result()
                        done.add(name)
                    except (Exception, SystemExit) as e:  # noqa: BLE001 - reported, then the run stops
                        say(str(e))
                        failed.append(name)
        if failed or todo:
            say(f"failed: {' '.join(failed)}; not run: {' '.join(todo)}")
            return 1
        return 0


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("checks", nargs="+")
    p.add_argument("--target", default=str(ROOT / "target" / "run-checks"), help="the shared /target")
    p.add_argument("--work", help="per-check scratch (default: TARGET/../run-checks-work)")
    p.add_argument("--ref", help="T_REF, the branch the run is for (main for main's dev channel)")
    p.add_argument("--jobs", type=int, default=4, help="checks run at once")
    p.add_argument("--force", action="store_true", help="run the named checks and their needs even if passed")
    p.add_argument("--dry-run", action="store_true", help="say what would run")
    p.add_argument("--no-limits", action="store_true", help="give checks every core and all memory")
    p.add_argument("--secret", action="append", default=[], metavar="NAME",
                   help="give checks that declare the secret NAME its value from this environment")
    p.add_argument("--given", action="append", default=[], metavar="CHECK",
                   help="take CHECK as passed: its outputs are in TARGET already, made elsewhere")
    p.add_argument("--keep-work", action="store_true", help="keep each check's scratch")
    args = p.parse_args(argv)
    args.work = args.work or str(Path(args.target).resolve().parent / "run-checks-work")
    return Runner(args).main()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
