#!/usr/bin/env python3
"""Run what the affected-packages check runs for a change, on the builders, concurrently.

  tools/ci/local-affected.sh [--base REF] [--stage STAGES] [--tapps] [--fail-fast]
  tools/ci/test-changed.sh [--base REF] [--stage STAGES] [-E FILTERSET] [-- TEST NAME FILTERS]

local-affected.sh checks a branch's commits as the batch check does: base defaults to
the merge-base with origin/main and the worktree must be clean. test-changed.sh is the
same run for a change still being edited: base defaults to the merge-base with
`integration` (origin/main without it), uncommitted and untracked files count, the
stages are clippy and test, and clippy covers the changed packages and their direct
dependants (the planner's `near`; `--closure` for the whole reverse-dependency closure
the batch check lints).

Selection is the planner's (tools/ci/affected_packages.py): the packages a change
affects, their checks and tests, the same filters CI uses. What differs from CI is only
how they run:

- One lint job and one test job, at the same time. Clippy (which type-checks every
  target it lints, so it is also the check) runs over every selected package at once,
  in a target directory of its own (`target/lint` on the builder) so it never waits on
  the tests' build lock; the tests run as one nextest run over every tested package, so
  the workspace compiles once and nextest's test groups and thread limits hold across
  all of it. Architectures that are only type-checked (`--full`) check on the arm
  builder at the same time.
- Each job runs where it is compiled: through `rcargo exec` (tools/ci/lint-affected.sh,
  tools/ci/t-nextest.sh) inside the builder's container, so helper binaries the tests
  need are built there and no test binary or artifact is fetched to this machine.
  Without rcargo the same commands run here.
- The tests that failed in the last run against the same base run first, alone, so a
  fix is confirmed (or not) within the time of its own tests; the rest follow without
  them. The record is per worktree and base (`<git-dir>/local-affected/`).
- A big test set is split by package between the x86 and the arm builder
  (`--split auto`, the default): the arm builder takes a quarter of the work (compile
  cost and recorded test time), each builder compiles and runs only its share.

Each job's whole output is in target/local-affected/<job>.log (each line stamped with the
seconds since its step started); the terminal gets
failures as they happen, a failed job's diagnostics and a summary of wall times.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
from ci import nextest_filters  # noqa: E402
from ci.affected_packages import package_cost, recorded_test_seconds  # noqa: E402

DEFAULT_STAGES = "check,clippy,test,goldens,certify,scenarios,scoped"
CHANGED_STAGES = "check,clippy,test"
GOLDEN_PACKAGES = {"shell", "tilo-ui", "ebook"}
# Above this much recorded test time (seconds, tools/ci/recorded_test_time.json) a
# test set is split between the builders; below it one builder finishes before a
# second one's compile would.
SPLIT_SECONDS = 900
# The second (arm) builder's share of a split test set: its 64 cores beside the x86 one's 192.
SECOND_SHARE = 0.25
# The packages kept on x86_64 when a test set is split: their code or tests differ by
# architecture and CI tests them on x86_64.
X86_ONLY = {"shell"}
# nextest's status line for a test that did not pass, and the test it names.
FAILED_LINE = re.compile(
    r"^\s*(?:TRY \d+ )?(FAIL|TIMEOUT|LEAK-FAIL|ABORT|SIG[A-Z]+)\s+\[[^\]]*\]\s+"
    r"(?:\(\s*\d+/\d+\)\s+)?(\S+)\s+(\S.*?)\s*$")
FORWARDED_LINE = re.compile(
    r"^\s*(?:TRY \d+ )?(FAIL|TIMEOUT|LEAK-FAIL|ABORT|SIG[A-Z]+)\s+\[|^error(\[E\d+\])?:|^\s*Summary \[")


def git(*args: str) -> str:
    return subprocess.check_output(["git", "-C", str(ROOT), *args], text=True).strip()


def ref_exists(ref: str) -> bool:
    return subprocess.run(["git", "-C", str(ROOT), "rev-parse", "--verify", "-q", f"{ref}^{{commit}}"],
                          stdout=subprocess.DEVNULL).returncode == 0


# --- selection -----------------------------------------------------------------------------

def plan_for(base: str, snapshot: bool, scratch: Path) -> tuple[dict, str, dict[str, int]]:
    """The planner's plan for HEAD (or a snapshot of the worktree) against `base`."""
    metadata_script = ROOT / "tools/ci/current_source_metadata.py"
    head = "HEAD"
    sealing: list[str] = []
    if snapshot:
        head = subprocess.check_output([sys.executable, str(metadata_script), "snapshot", "--root", str(ROOT)],
                                       text=True).strip()
        if head != git("rev-parse", "HEAD"):
            sealing = ["--snapshot", head]
    cargo_json = scratch / "cargo.json"
    with cargo_json.open("w") as out:
        subprocess.run(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
                       cwd=ROOT, stdout=out, check=True)
    for mode, source, target in (("seal", cargo_json, scratch / "bundle.json"),
                                 ("validate", scratch / "bundle.json", scratch / "metadata.json")):
        subprocess.run([sys.executable, str(metadata_script), mode, "--root", str(ROOT), "--input", str(source),
                        "--output", str(target), *sealing], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
    plan = subprocess.check_output(
        [sys.executable, str(ROOT / "tools/ci/affected_packages.py"), "--metadata", str(scratch / "metadata.json"),
         "--base", base, "--head", head, "--shards", "2", "--allow-security-skip"], cwd=ROOT, text=True)
    packages = json.loads((scratch / "metadata.json").read_text())["packages"]
    return json.loads(plan), head, {pkg["name"]: package_cost(pkg) for pkg in packages}


def names(packages: str) -> list[str]:
    """`-p a -p b` -> [a, b]."""
    words = shlex.split(packages)
    return [words[i + 1] for i, word in enumerate(words[:-1]) if word in ("-p", "--package")]


def flags(packages: list[str]) -> list[str]:
    return [word for name in packages for word in ("-p", name)]


def ordered_union(rows: list[dict], key: str) -> list[str]:
    seen: dict[str, None] = {}
    for row in rows:
        for name in names(str(row.get(key, ""))):
            seen.setdefault(name, None)
    return list(seen)


# --- builders ------------------------------------------------------------------------------

class Builder:
    """Where a job runs: an rcargo builder (`rcargo exec` in its build container), or here."""

    def __init__(self, name: str, config: Path | None):
        self.name = name
        self.config = config

    def prepare(self) -> None:
        """Sync the worktree and ready the build container before the jobs start: jobs started
        together would otherwise queue on each other's sync and container preparation, and a
        session that planned its sync before another's landed compares every file again."""
        if self.config is None or Job.dry_run:
            return
        command, env = self.command(["true"])
        start = time.monotonic()
        result = subprocess.run(command, cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                text=True, errors="replace")
        (ROOT / "target" / "local-affected" / f"prepare-{self.name}.log").write_text(result.stdout)
        if result.returncode != 0:
            raise RuntimeError(f"preparing the {self.name} builder failed:\n{result.stdout}")
        say(f"==> {self.name} builder: worktree synced, build container ready ({time.monotonic() - start:.0f} s)")

    def command(self, argv: list[str]) -> tuple[list[str], dict[str, str]]:
        env = dict(os.environ)
        if self.config is None:
            return argv, env
        env["RCARGO_CONFIG"] = str(self.config)
        return ["rcargo", "exec", "--", *argv], env


def builders(local: bool) -> tuple[Builder, Builder | None]:
    """The test builder (the one `cargo test` goes to) and a second one to share a big test
    set with. The cargo shim's: ~/.config/rcargo/x86.toml for tests, config.toml (arm)."""
    if local or not shutil.which("rcargo"):
        return Builder("local", None), None
    home = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "rcargo"
    first = Path(os.environ.get("LOCAL_AFFECTED_RCARGO_CONFIG", home / "x86.toml"))
    second = Path(os.environ.get("LOCAL_AFFECTED_RCARGO_CONFIG_2", home / "config.toml"))
    if not first.is_file():
        return Builder("local", None), None
    return Builder("x86", first), (Builder("arm", second) if second.is_file() and second != first else None)


# --- jobs ----------------------------------------------------------------------------------

class Job:
    dry_run = False

    def __init__(self, label: str, logs: Path):
        self.label = label
        self.log = logs / f"{label}.log"
        self.steps: list[tuple[str, int, float]] = []
        self.failed_tests: set[tuple[str, str]] = set()
        self.ran_to_end = True
        self.started = time.monotonic()
        self.ended = self.started
        if not self.dry_run:  # a dry run beside a real one leaves its logs alone
            self.log.write_text("")

    def ok(self) -> bool:
        return all(code == 0 for _, code, _ in self.steps)

    def run(self, step: str, builder: Builder, argv: list[str]) -> int:
        command, env = builder.command(argv)
        say(f"[{self.label}] {step} ({builder.name})")
        if self.dry_run:
            say(f"    {shlex.join(command)}")
            self.steps.append((step, 0, 0.0))
            return 0
        start = time.monotonic()
        failed: set[tuple[str, str]] = set()
        with self.log.open("a") as log:
            log.write(f"==> {step} ({builder.name}): {shlex.join(command)}\n")
            log.flush()
            with subprocess.Popen(command, cwd=ROOT, env=env, stdout=subprocess.PIPE,
                                  stderr=subprocess.STDOUT, text=True, errors="replace", bufsize=1) as process:
                assert process.stdout is not None
                for line in process.stdout:
                    log.write(f"{time.monotonic() - start:7.1f} {line}")
                    match = FAILED_LINE.match(line)
                    if match:
                        failed.add((match.group(2), match.group(3)))
                    if FORWARDED_LINE.search(line):
                        say(f"[{self.label}] {line.rstrip()}")
                code = process.wait()
            elapsed = time.monotonic() - start
            log.write(f"==> {step}: exit {code} after {elapsed:.0f} s\n")
        self.failed_tests |= failed
        # nextest exits 100 when tests failed and the run finished; anything else that is not
        # success (a build or listing error, a cancelled run) leaves tests unrun.
        if code not in (0, 100):
            self.ran_to_end = False
        self.steps.append((step, code, elapsed))
        self.ended = time.monotonic()
        return code

    def tail(self, lines: int = 120) -> str:
        # Without libtest's JSON events (t-nextest.sh records test times from them).
        text = [line for line in self.log.read_text(errors="replace").splitlines()
                if not line[8:].lstrip().startswith(("{", "}", '"'))]
        return "\n".join(text[-lines:])


SAY = threading.Lock()


def say(message: str) -> None:
    with SAY:
        print(message, flush=True)


# --- failed-first record ---------------------------------------------------------------------

def record_path(base: str) -> Path:
    git_dir = Path(git("rev-parse", "--absolute-git-dir"))
    key = re.sub(r"[^A-Za-z0-9._-]", "_", base)
    return git_dir / "local-affected" / f"failed-{key}.json"


def read_record(path: Path) -> set[tuple[str, str]]:
    if not path.exists():
        return set()
    data = json.loads(path.read_text())
    return {(entry["binary_id"], entry["test"]) for entry in data["failed"]}


def write_record(path: Path, failed: set[tuple[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"failed": [{"binary_id": b, "test": t} for b, t in sorted(failed)]}, indent=1) + "\n")


def filterset_of(tests: set[tuple[str, str]]) -> str:
    return " or ".join(f"(binary_id(={binary}) and test(={test}))" for binary, test in sorted(tests))


def package_of(binary_id: str) -> str:
    return binary_id.split("::", 1)[0]


# --- the run -------------------------------------------------------------------------------

def tapps(job: Job, local: Builder, base: str, scratch: Path) -> None:
    """Package the affected size-budgeted tapps (a release build) and check their budgets."""
    release = subprocess.check_output([sys.executable, "tools/apps/app_release.py", "--base", base],
                                      cwd=ROOT, text=True)
    affected = " ".join(json.loads(release)["apps"])
    out = scratch / "tapps.out"
    subprocess.run([sys.executable, "tools/perf/tapp_size_ratchet.py", "select", "--apps", affected,
                    "--github-output", str(out)], cwd=ROOT, check=True)
    apps = next((line.split("=", 1)[1] for line in out.read_text().splitlines()
                 if line.startswith("tapp_apps=")), "")
    shutil.rmtree(scratch, ignore_errors=True)
    if not apps:
        say("[post] no tapps selected")
        return
    # As the tapps job: release profile with thin LTO, 16 codegen units.
    job.run(f"package tapps ({apps})", local,
            ["env", "CARGO_PROFILE_RELEASE_LTO=thin", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16",
             f"TILO_RELEASE_APPS={apps}", "dist/apps/package-apps.sh"])
    job.run("tapp size budgets", local,
            [sys.executable, "tools/perf/tapp_size_ratchet.py", "measure", "dist/app-release", "--arch",
             os.uname().machine])


def _capture(function, errors: list[BaseException]) -> None:
    try:
        function()
    except BaseException as error:  # reported by the caller
        errors.append(error)


def split_packages(packages: list[str], seconds: dict[str, int], costs: dict[str, int],
                   share: float = SECOND_SHARE) -> tuple[list[str], list[str]]:
    """The test packages for the first builder and for the second, which takes about `share`
    of the work: half of a package's weight is its compile (the planner's structural cost),
    half its recorded test time. X86_ONLY stay with the first."""
    total_cost = sum(costs.get(n, 0) for n in packages) or 1
    total_seconds = sum(seconds.get(n, 0) for n in packages) or 1
    weight = {n: costs.get(n, 0) / total_cost + seconds.get(n, 0) / total_seconds for n in packages}
    first = [n for n in packages if n in X86_ONLY]
    second: list[str] = []
    budget = share * sum(weight.values())
    taken = 0.0
    for name in sorted((n for n in packages if n not in X86_ONLY), key=lambda n: -weight[n]):
        if taken + weight[name] <= budget:
            second.append(name)
            taken += weight[name]
        else:
            first.append(name)
    return first, second


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--changed", action="store_true",
                        help="test-changed.sh: a change being edited (worktree snapshot, base integration)")
    parser.add_argument("--base", help="comparison base (default: merge-base with origin/main; "
                                       "with --changed, with integration)")
    parser.add_argument("--stage", help=f"comma list of stages (default: {DEFAULT_STAGES}; "
                                        f"with --changed: {CHANGED_STAGES}); also tapps")
    parser.add_argument("--tapps", action="store_true", help="also package and size-check the affected tapps")
    parser.add_argument("--closure", action="store_true",
                        help="with --changed: lint the whole reverse-dependency closure, as the batch "
                             "check does (default: the changed packages and their direct dependants)")
    parser.add_argument("--fail-fast", action="store_true",
                        help="stop the other jobs once one fails (the default runs every job to its end)")
    parser.add_argument("--keep-going", action="store_true", help=argparse.SUPPRESS)  # the default now
    parser.add_argument("--split", choices=("auto", "always", "never"), default="auto",
                        help="share the test packages between the x86 and arm builders (auto: when "
                             f"their recorded test time exceeds {SPLIT_SECONDS} s)")
    parser.add_argument("--local", action="store_true", help="run every job on this machine")
    parser.add_argument("--dry-run", action="store_true", help="print what would run, run nothing")
    parser.add_argument("-E", "--filterset", help="only tests matching this nextest filterset as well")
    parser.add_argument("names", nargs="*", help="test name filters, passed to nextest after --")
    args = parser.parse_args()

    stages = set((args.stage or (CHANGED_STAGES if args.changed else DEFAULT_STAGES)).split(","))
    if args.tapps:
        stages.add("tapps")
    want = stages.__contains__
    Job.dry_run = args.dry_run

    base = args.base
    if not base:
        base = "integration" if args.changed and ref_exists("integration") else "origin/main"
    base_commit = git("merge-base", base, "HEAD")
    logs = ROOT / "target" / "local-affected"
    logs.mkdir(parents=True, exist_ok=True)

    started = time.monotonic()
    with tempfile.TemporaryDirectory(dir=logs) as scratch:
        try:
            plan, head, costs = plan_for(base_commit, args.changed, Path(scratch))
        except subprocess.CalledProcessError as error:
            say(f"select failed: {error}" + ("" if args.changed else
                "; a change still being edited is tested with tools/ci/test-changed.sh"))
            return 1
    linted = len(plan["near"]) if args.changed and not args.closure else plan["selected_count"]
    say(f"==> select: base {base} ({base_commit[:12]}), {plan['selected_count']} of {plan['workspace_count']} "
        f"packages selected ({plan['reason']}), {linted} linted, {len(plan['tested'])} tested, "
        f"{time.monotonic() - started:.0f} s")

    filters = nextest_filters.filters()
    filter_env = [f"{key}={value}" for key, value in filters.items()]
    golden, push_only = filters["GOLDEN_FILTER"], filters["PUSH_ONLY_FILTER"]
    pull_filter = f"not ({golden}) and not ({push_only})"
    if args.filterset:
        pull_filter = f"({pull_filter}) and ({args.filterset})"
    tail = ["--", *args.names] if args.names else []

    clippy = ordered_union(plan["clippy_matrix"]["include"], "packages")
    if args.changed and not args.closure:
        # A change being edited: lint what is one edge from it (the planner's `near`); the
        # batch check lints the whole closure.
        near = set(plan["near"])
        clippy = [name for name in clippy if name in near]
    checks = [row for row in plan["check_matrix"]["include"]]
    rows = plan["matrix"]["include"]
    tests = ordered_union(rows, "test_packages")

    primary, secondary = builders(args.local)
    stop = threading.Event()
    jobs: list[Job] = []
    threads: list[threading.Thread] = []

    def spawn(label: str, body) -> None:
        job = Job(label, logs)
        jobs.append(job)

        def runner() -> None:
            try:
                body(job)
            finally:
                job.ended = time.monotonic()
                if args.fail_fast and not job.ok():
                    stop.set()
        thread = threading.Thread(target=runner, name=label)
        threads.append(thread)
        thread.start()

    record = record_path(base)
    previous = read_record(record)
    seconds = recorded_test_seconds()
    shares: list[tuple[Builder, list[str]]] = [(primary, tests)]
    if want("test") and tests and secondary is not None and args.split != "never":
        total = sum(seconds.get(name, 0) for name in tests)
        if args.split == "always" or total > SPLIT_SECONDS:
            first, second = split_packages(tests, seconds, costs)
            if first and second:
                shares = [(primary, first), (secondary, second)]

    used = [primary] + [builder for builder, _ in shares[1:]]
    if want("check") and checks and secondary is not None and secondary not in used:
        used.append(secondary)
    errors: list[BaseException] = []
    preparing = [threading.Thread(target=_capture, args=(builder.prepare, errors)) for builder in used]
    for thread in preparing:
        thread.start()
    for thread in preparing:
        thread.join()
    try:
        if errors:
            raise errors[0]
    except RuntimeError as error:
        say(str(error))
        return 1

    # Lint: clippy over the whole selection, the shipped-default builds the planner names,
    # the portable and headless library builds; one builder session, target/lint.
    if want("clippy") and clippy:
        lint_args = list(flags(clippy))
        extra = []
        if "tilo-ui" in clippy and want("check"):
            extra.append("--portable-ui")
        if "tilo-app" in clippy and want("check"):
            extra.append("--headless-app")
        spawn("lint", lambda job: job.run("clippy", primary,
                                          ["bash", "tools/ci/lint-affected.sh", *extra, "--", *lint_args]))
        # A package tested with features its shipped build lacks is also linted as it ships:
        # those builds beside the main lint, in a target directory of their own.
        defaults = subprocess.check_output([sys.executable, "tools/ci/package_features.py", "--default-builds",
                                            *lint_args], cwd=ROOT, text=True).split()
        if defaults:
            spawn("lint-defaults", lambda job: job.run(
                f"clippy over the default builds of {' '.join(defaults)}", primary,
                ["bash", "tools/ci/lint-affected.sh", "--default-builds", *defaults]))

    # Type-check only (other architectures, `--full`): on the arm builder when there is one.
    if want("check") and checks:
        def check(job: Job) -> None:
            for row in checks:
                where = secondary if row.get("arch") == "aarch64" and secondary else primary
                if stop.is_set():
                    return
                job.run(f"check {row.get('arch', '')} shard {row['shard']}", where,
                        ["bash", "tools/ci/lint-affected.sh", "--check-only", "--", *flags(names(row["packages"]))])
        spawn("check", check)

    def nextest(packages: list[str], filterset: str, extra: list[str] | None = None) -> list[str]:
        return ["env", *filter_env, "bash", "tools/ci/t-nextest.sh", "cargo", "nextest", "run", "--locked",
                "--profile", "ci", "--tests", *flags(packages), "-E", filterset, *(extra or []), *tail]

    if want("test") and tests:
        failing_first = {test for test in previous if package_of(test[0]) in set(tests)}

        def test_share(builder: Builder, packages: list[str], first: set[tuple[str, str]]):
            def body(job: Job) -> None:
                remaining = pull_filter
                if first:
                    # Over the same packages as the full run, so both resolve features alike and
                    # the full run finds every test binary already built.
                    job.run(f"{len(first)} test(s) that failed last time", builder,
                            nextest(packages, f"({pull_filter}) and ({filterset_of(first)})", ["--no-tests=pass"]))
                    remaining = f"({pull_filter}) and not ({filterset_of(first)})"
                if stop.is_set():
                    return
                # A package whose only selected tests ran first has nothing left: pass.
                job.run(f"nextest ({len(packages)} packages)", builder,
                        nextest(packages, remaining, ["--no-tests=pass"] if first else []))
                if want("goldens") and GOLDEN_PACKAGES & set(packages) and not stop.is_set():
                    job.run("visual goldens", builder,
                            nextest(sorted(GOLDEN_PACKAGES & set(packages)), golden, ["--no-tests=pass"]))
            return body

        for index, (builder, packages) in enumerate(shares):
            mine = {test for test in failing_first if package_of(test[0]) in set(packages)}
            spawn("test" if len(shares) == 1 else f"test-{builder.name}", test_share(builder, packages, mine))

    for thread in threads:
        thread.join()

    # What runs after (and only where) it always did: certify, scenarios, scoped readers, tapps.
    post = Job("post", logs) if any(want(s) for s in ("certify", "scenarios", "scoped", "tapps")) else None
    if post is not None and not stop.is_set():
        jobs.append(post)
        local = Builder("local", None)
        for row in rows:
            row_tests = names(row.get("test_packages", ""))
            if want("certify") and row.get("certify") and row_tests:
                post.run(f"tilo-app certify shard {row['shard']}", local,
                         ["env", "TILO_REQUIRE_GPU=1", "cargo", "run", "--locked", "-p", "tilo-app-cli", "--bin",
                          "tilo-app", "--", "certify", "--workspace", *flags(row_tests)])
            if want("scenarios") and any(n.startswith("example-") or n == "tilo-app-cli" for n in row_tests):
                post.run(f"example scenarios shard {row['shard']}", local,
                         ["bash", "tools/test-env.sh", "bash", "tools/ci/example-scenarios.sh", *flags(row_tests)])
            if want("scoped") and row.get("scoped_tests"):
                post.run(f"scoped reader tests shard {row['shard']}", local,
                         ["env", f"SCOPED_TESTS={row['scoped_tests']}", sys.executable,
                          "tools/ci/run_scoped_tests.py"])
        if want("tapps"):
            tapps(post, local, base_commit, Path(tempfile.mkdtemp(dir=logs)))

    # The record: what failed now; a run that ended early keeps what it could not rerun.
    test_jobs = [job for job in jobs if job.label.startswith("test")]
    if test_jobs and not args.dry_run:
        failed = set().union(*(job.failed_tests for job in test_jobs))
        if not all(job.ran_to_end for job in test_jobs):
            failed |= previous
        write_record(record, failed)

    say("==> summary")
    for job in jobs:
        for step, code, elapsed in job.steps:
            say(f"    {'ok    ' if code == 0 else 'FAILED'} {elapsed:6.0f} s  {job.label}: {step}")
    say(f"    wall time {time.monotonic() - started:.0f} s; logs in {logs.relative_to(ROOT)}/")
    failed_jobs = [job for job in jobs if not job.ok()]
    for job in failed_jobs:
        if job.failed_tests:
            say(f"==> {job.label}: {len(job.failed_tests)} failed test(s); rerun them first next time, "
                f"output in {job.log.relative_to(ROOT)}:")
            for binary, test in sorted(job.failed_tests):
                say(f"    {binary} {test}")
        else:
            say(f"==> {job.label} failed; the end of {job.log.relative_to(ROOT)}:")
        say(job.tail())
    return 1 if failed_jobs else 0


if __name__ == "__main__":
    sys.exit(main())
