#!/usr/bin/env python3
"""Same-job A/B: prove a performance change despite runner noise (TIL-738).

The scheduled lane compares a run with earlier nights on other hosts, and on
the shared `runner` class that spread is larger than most changes (the same
relay bench measured 15.9 and 31.2 CPU-seconds per collaborator-hour on two
runs). This tool builds two commits in one checkout, one after the other, and
runs the existing benchmark of a KPI group on both, alternating, in one job
on one machine. A slow host then slows both sides alike, and pairing each
round's two runs cancels it.

    python3 tools/perf/ab.py run --groups hosting --base origin/main --candidate HEAD \\
        --rounds 8 --out target/perf-ab

1. **Build.** The tree must have no tracked changes. For each side the
   checkout is detached at that side's commit, the group's existing build
   commands run (the same ones as `.forgejo/workflows/perf-scheduled.yml`),
   and the binaries are copied to `OUT/bin/<side>/<group>/`. Test executables
   are found through `cargo test --no-run --message-format=json`. Both sides
   share the one target directory, so the second build recompiles only what
   differs. The checkout then returns to where it was: that commit's harness
   (`scripts/perf/`, the adapters) drives both binaries.
2. **Run.** N rounds, both sides per round, in balanced order: odd rounds run
   base first, even rounds candidate first, so a drift over the job (a
   neighbour's build starting, a cache warming) falls on both sides equally.
   Each run produces records through the group's existing adapter, with the
   side's commit and `meta.ab_side`, `meta.ab_round`.
3. **Analyse.** Per KPI and percentile (p50, p99), the rounds where both
   sides ran are paired. The statistic is the paired log-ratio
   ln(candidate / base) (the difference when a value is 0): the
   Hodges-Lehmann estimate (median of the Walsh averages) with its
   distribution-free confidence interval from the exact Wilcoxon signed-rank
   distribution, and the exact two-sided sign test beside it. The verdict, for
   a KPI where lower is better (every unit but fps):

   - `improved`: the whole interval is below 1 (below 0 as a difference);
   - `regressed`: the whole interval is above it;
   - `unchanged`: both sides measured exactly the same in every round;
   - `inconclusive`: anything else, including fewer than six paired rounds,
     the fewest that can reach 95 % at all.

   Records, `ab-result.json` and `summary.md` are written to OUT. A KPI one
   side does not report (a metric the candidate adds) is listed, not paired.

A/B results are evidence for a pull request, never a baseline: nothing here
writes to the results store.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import math
import os
import shutil
import statistics
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Any, Callable

sys.path.insert(0, str(Path(__file__).resolve().parent))

import adapters  # noqa: E402
import record  # noqa: E402

SCHEMA = 1
SIDES = ("base", "candidate")
STATS = ("p50", "p99")
CONFIDENCE = 0.95
# Units where a larger value is the better one; every other unit is a cost.
HIGHER_IS_BETTER = frozenset({"fps"})
VERDICTS = ("improved", "regressed", "unchanged", "inconclusive")


# --- statistics ---------------------------------------------------------------

def signed_rank_null(n: int) -> list[int]:
    """How many of the 2**n sign patterns give each Wilcoxon T+ (no ties)."""
    counts = [1]
    for rank in range(1, n + 1):
        grown = counts + [0] * rank
        for total, ways in enumerate(counts):
            grown[total + rank] += ways
        counts = grown
    return counts


def walsh_averages(values: list[float]) -> list[float]:
    return sorted((values[i] + values[j]) / 2 for i in range(len(values)) for j in range(i, len(values)))


@dataclasses.dataclass
class Interval:
    estimate: float
    low: float | None
    high: float | None
    # The interval's exact coverage (at least the requested level), or None
    # when n is too small to reach it.
    confidence: float | None


def hodges_lehmann(values: list[float], level: float = CONFIDENCE) -> Interval:
    """The Hodges-Lehmann location estimate of paired differences and its
    distribution-free confidence interval (Walsh averages, exact signed-rank
    critical value)."""
    if not values:
        raise ValueError("no paired values")
    walsh = walsh_averages(values)
    estimate = statistics.median(walsh)
    counts = signed_rank_null(len(values))
    total = 2 ** len(values)
    tail = (1 - level) / 2
    cumulative = 0
    critical = None
    for value, ways in enumerate(counts):
        cumulative += ways
        if cumulative / total > tail:
            break
        critical = value
    if critical is None:
        return Interval(estimate, None, None, None)
    below = sum(counts[:critical + 1]) / total
    return Interval(estimate, walsh[critical], walsh[len(walsh) - 1 - critical], 1 - 2 * below)


def sign_test(values: list[float]) -> tuple[int, int, int, float]:
    """(negative, positive, zero, exact two-sided p) of paired differences."""
    negative = sum(value < 0 for value in values)
    positive = sum(value > 0 for value in values)
    zero = len(values) - negative - positive
    n = negative + positive
    if n == 0:
        return negative, positive, zero, 1.0
    extreme = min(negative, positive)
    p = 2 * sum(math.comb(n, k) for k in range(extreme + 1)) / 2 ** n
    return negative, positive, zero, min(1.0, p)


def min_rounds(level: float = CONFIDENCE) -> int:
    """The fewest pairs whose signed-rank interval can reach `level`."""
    n = 1
    while 2 / 2 ** n > 1 - level:
        n += 1
    return n


@dataclasses.dataclass
class Paired:
    kpi: str
    unit: str
    stat: str
    rounds: list[int]
    base: list[float]
    candidate: list[float]
    # "ratio": estimate and interval are candidate/base; "difference":
    # candidate - base in the KPI's unit (a side measured 0).
    scale: str
    estimate: float
    low: float | None
    high: float | None
    confidence: float | None
    better_rounds: int
    worse_rounds: int
    tied_rounds: int
    sign_p: float
    verdict: str
    note: str = ""

    def to_json(self) -> dict[str, Any]:
        return dataclasses.asdict(self)


def judge(kpi: str, unit: str, stat: str, rounds: list[int], base: list[float],
          candidate: list[float], level: float = CONFIDENCE) -> Paired:
    """Pair one KPI's percentile across rounds and give its verdict."""
    ratio = all(value > 0 for value in (*base, *candidate))
    if ratio:
        diffs = [math.log(c / b) for b, c in zip(base, candidate, strict=True)]
    else:
        diffs = [c - b for b, c in zip(base, candidate, strict=True)]
    interval = hodges_lehmann(diffs, level)
    negative, positive, zero, p = sign_test(diffs)
    higher_better = unit in HIGHER_IS_BETTER
    better, worse = (positive, negative) if higher_better else (negative, positive)

    def shown(value: float | None) -> float | None:
        if value is None:
            return None
        return math.exp(value) if ratio else value

    note = ""
    if all(value == 0 for value in diffs):
        verdict = "unchanged"
        note = f"identical in all {len(diffs)} rounds"
    elif interval.low is None:
        verdict = "inconclusive"
        note = f"{len(diffs)} paired rounds; {min_rounds(level)} are needed for a {level:.0%} interval"
    elif interval.high < 0 or interval.low > 0:
        lower = interval.high < 0
        verdict = "improved" if lower != higher_better else "regressed"
    else:
        verdict = "inconclusive"
        note = "the interval includes no change"
    return Paired(kpi, unit, stat, rounds, base, candidate, "ratio" if ratio else "difference",
                  shown(interval.estimate), shown(interval.low), shown(interval.high), interval.confidence,
                  better, worse, zero, p, verdict, note)


@dataclasses.dataclass
class Analysis:
    paired: list[Paired]
    only: dict[str, list[str]]
    rounds: int


def analyse(records: list[record.Record], level: float = CONFIDENCE) -> Analysis:
    """Pair the run's records by KPI and round."""
    table: dict[str, dict[int, dict[str, record.Record]]] = {}
    units: dict[str, str] = {}
    seen: dict[str, set[str]] = {side: set() for side in SIDES}
    rounds = set()
    for item in records:
        side, round_ = item.meta.get("ab_side"), item.meta.get("ab_round")
        if side not in SIDES or round_ is None:
            raise ValueError(f"{item.kpi}: not an A/B record (meta ab_side/ab_round missing)")
        number = int(round_)
        rounds.add(number)
        slot = table.setdefault(item.kpi, {}).setdefault(number, {})
        if side in slot:
            raise ValueError(f"{item.kpi}: two {side} records in round {number}")
        if units.setdefault(item.kpi, item.unit) != item.unit:
            raise ValueError(f"{item.kpi}: reported in {units[item.kpi]} and {item.unit}")
        slot[side] = item
        seen[side].add(item.kpi)
    paired = []
    for kpi in sorted(set.intersection(*seen.values())):
        complete = sorted(number for number, slot in table[kpi].items() if len(slot) == 2)
        if not complete:
            continue
        for stat in STATS:
            base = [getattr(table[kpi][number]["base"], stat) for number in complete]
            candidate = [getattr(table[kpi][number]["candidate"], stat) for number in complete]
            paired.append(judge(kpi, units[kpi], stat, complete, base, candidate, level))
    only = {side: sorted(seen[side] - seen[other]) for side, other in zip(SIDES, reversed(SIDES))}
    return Analysis(paired, only, len(rounds))


# --- report -------------------------------------------------------------------

def number(value: float) -> str:
    return f"{value:,.0f}" if abs(value) >= 10_000 else f"{value:.4g}"


def change(item: Paired, value: float | None) -> str:
    if value is None:
        return "–"
    if item.scale == "ratio":
        return f"{(value - 1) * 100:+.1f} %"
    return f"{value:+.4g} {item.unit}"


def summary(analysis: Analysis, sides: dict[str, dict[str, str]], groups: list[str], harness: str,
            failures: list[str]) -> str:
    base, candidate = sides["base"], sides["candidate"]
    lines = [f"## A/B: {', '.join(groups)}", "",
             f"base `{base['commit'][:10]}` ({base['ref']}) vs candidate `{candidate['commit'][:10]}` "
             f"({candidate['ref']}), {analysis.rounds} rounds in one job, balanced order (odd rounds "
             f"base first). Harness `{harness[:10]}`. Change is candidate against base: the "
             f"Hodges-Lehmann estimate of the paired ratio, with its {CONFIDENCE:.0%} interval.", ""]
    decided = {verdict: [item.kpi for item in analysis.paired if item.stat == "p50" and item.verdict == verdict]
               for verdict in ("improved", "regressed")}
    for verdict, kpis in decided.items():
        if kpis:
            lines.append(f"**{verdict}** (p50): " + ", ".join(f"`{kpi}`" for kpi in kpis))
    if any(decided.values()):
        lines.append("")
    lines += ["| KPI | stat | base median | candidate median | change | interval | rounds better / worse "
              "| sign p | verdict |", "|---|---|---|---|---|---|---|---|---|"]
    for item in analysis.paired:
        interval = (f"[{change(item, item.low)}, {change(item, item.high)}]"
                    if item.low is not None else "–")
        verdict = f"**{item.verdict}**" if item.verdict in ("improved", "regressed") else item.verdict
        if item.note:
            verdict += f" ({item.note})"
        lines.append(
            f"| `{item.kpi}` | {item.stat} | {number(statistics.median(item.base))} {item.unit} "
            f"| {number(statistics.median(item.candidate))} {item.unit} | {change(item, item.estimate)} "
            f"| {interval} | {item.better_rounds} / {item.worse_rounds} of {len(item.rounds)} "
            f"| {item.sign_p:.3g} | {verdict} |")
    for side, kpis in analysis.only.items():
        if kpis:
            lines += ["", f"Only the {side} reports: " + ", ".join(f"`{kpi}`" for kpi in kpis)]
    if failures:
        lines += ["", "Failed runs (their rounds are not paired):"] + [f"- {failure}" for failure in failures]
    return "\n".join(lines) + "\n"


def result_json(analysis: Analysis, sides: dict[str, dict[str, str]], groups: list[str], harness: str,
                failures: list[str]) -> dict[str, Any]:
    return {"schema": SCHEMA, "groups": groups, "base": sides["base"], "candidate": sides["candidate"],
            "harness": harness, "rounds": analysis.rounds, "confidence": CONFIDENCE,
            "order": "balanced: odd rounds base first, even rounds candidate first",
            "kpis": [item.to_json() for item in analysis.paired],
            "only_base": analysis.only["base"], "only_candidate": analysis.only["candidate"],
            "failed_runs": failures}


def write_report(out: Path, records: list[record.Record], sides: dict[str, dict[str, str]],
                 groups: list[str], harness: str, failures: list[str]) -> tuple[Analysis, str]:
    analysis = analyse(records)
    text = summary(analysis, sides, groups, harness, failures)
    (out / "summary.md").write_text(text, encoding="utf-8")
    (out / "ab-result.json").write_text(
        json.dumps(result_json(analysis, sides, groups, harness, failures), indent=2) + "\n", encoding="utf-8")
    return analysis, text


def provenance(records: list[record.Record]) -> tuple[dict[str, dict[str, str]], list[str], str]:
    """The sides, groups and harness an earlier run's records name."""
    sides = {}
    for item in records:
        sides.setdefault(item.meta["ab_side"], {"ref": item.meta.get("ab_ref", "?"), "commit": item.commit})
    missing = [side for side in SIDES if side not in sides]
    if missing:
        raise ValueError(f"the records hold no {', '.join(missing)} run")
    harness = next((item.meta["ab_harness"] for item in records if "ab_harness" in item.meta), "unknown")
    return sides, sorted({item.group for item in records}), harness


# --- groups -------------------------------------------------------------------

@dataclasses.dataclass
class RunContext:
    repo: Path
    # The side's artifacts, by name; a test executable's package directory is
    # its working directory, as under `cargo test`.
    bins: dict[str, Path]
    cwd: dict[str, Path]
    dir: Path
    env: dict[str, Any]
    env_json: Path
    options: argparse.Namespace


@dataclasses.dataclass(frozen=True)
class Group:
    name: str
    profile: str
    renderer: str
    # Build commands, from the repository root, as perf-scheduled.yml runs them.
    builds: tuple[tuple[str, ...], ...]
    # Artifact name -> path below the repository after the builds.
    binaries: dict[str, str]
    # Artifact name -> (`cargo test --no-run` command, test target name).
    tests: dict[str, tuple[tuple[str, ...], str]]
    # Artifact name -> path it must occupy while its side runs: a path a test
    # executable baked in at compile time (`env!("CARGO_BIN_EXE_shell")`).
    stage: dict[str, str]
    run: Callable[[RunContext], list[record.Record]]
    # Artifact name -> (source file, build command, path after the build): a
    # benchmark built only on a side whose commit has its source, so a base
    # older than the benchmark still runs; its KPIs are then the candidate's
    # alone, listed but not paired.
    optional: dict[str, tuple[str, tuple[str, ...], str]] = dataclasses.field(default_factory=dict)


def call(command: list[str], log: Path, cwd: Path, env: dict[str, str] | None = None,
         check: bool = True) -> int:
    """Run one command with its output in `log`; on failure, show the tail."""
    with log.open("ab") as stream:
        status = subprocess.run([str(part) for part in command], cwd=cwd, stdout=stream,
                                stderr=subprocess.STDOUT, env={**os.environ, **(env or {})},
                                check=False).returncode
    if status != 0 and check:
        tail = log.read_text(errors="replace").splitlines()[-40:]
        raise RuntimeError(f"{command[0]} exited {status}; last lines of {log}:\n" + "\n".join(tail))
    return status


def run_hosting(ctx: RunContext) -> list[record.Record]:
    # Both sides need a bench that knows --relay-cpus; an older one refuses it.
    cpus = ["--relay-cpus", str(ctx.options.hosting_relay_cpus)] if ctx.options.hosting_relay_cpus else []
    call([ctx.bins["relay_load"], "--collaborators", "100", "--minutes", str(ctx.options.hosting_minutes),
          *cpus, "--out", ctx.dir], ctx.dir / "bench.log", ctx.repo)
    report = json.loads((ctx.dir / "hosting-load.json").read_text(encoding="utf-8"))
    prices = tomllib.loads(adapters.PRICES.read_text(encoding="utf-8"))
    return adapters.hosting_load(report, prices, ctx.env, require_all=False)


def run_latency(ctx: RunContext) -> list[record.Record]:
    records = ctx.dir / "records.jsonl"
    call([sys.executable, "scripts/perf/latency_bench.py", "--shell-bin", ctx.bins["shell"],
          "--out", ctx.dir / "run", "--env-json", ctx.env_json, "--records", records],
         ctx.dir / "bench.log", ctx.repo)
    return record.read(records)


def run_agent(ctx: RunContext) -> list[record.Record]:
    records = ctx.dir / "records.jsonl"
    call(["bash", "tools/with-test-environment.sh", sys.executable, "scripts/perf/agent_rtt.py",
          "--spawn", ctx.bins["shell"], "--samples", "1000", "--warmup", "100",
          "--transport", "test-control socket, headless, release + dev-overrides, thin LTO",
          "--env-json", ctx.env_json, "--out", records, "--log", ctx.dir / "compositor.log"],
         ctx.dir / "bench.log", ctx.repo)
    return record.read(records)


def run_render(ctx: RunContext) -> list[record.Record]:
    records = ctx.dir / "records.jsonl"
    pans = ["--strip-pan-bench", ctx.bins["strip_pan_bench"]] if "strip_pan_bench" in ctx.bins else []
    call([sys.executable, "scripts/perf/render_bench.py", "timing", ctx.bins["ui-perf"],
          ctx.bins["redraw_bench"], "--env-json", ctx.env_json, "--out", ctx.dir, *pans, "--records", records],
         ctx.dir / "bench.log", ctx.repo)
    opening = ctx.dir / "framework-open.log"
    call([ctx.bins["framework-scaling"], "5000,50000", "open"], opening, ctx.repo)
    return record.read(records) + adapters.framework_open(
        [opening.read_text(encoding="utf-8", errors="replace")], ctx.env)


IDLE_WAKEUPS_TEST = "idle_wakeups::an_idle_desktop_with_two_apps_stays_within_its_wakeup_budget"


def run_wakeups(ctx: RunContext) -> list[record.Record]:
    log = ctx.dir / "idle.log"
    # A run over the wakeup budget fails the test but still measured: the
    # adapter fails only when the run printed no measurement.
    call(["bash", ctx.repo / "tools/test-env.sh", ctx.bins["it"], IDLE_WAKEUPS_TEST, "--exact", "--nocapture"],
         log, ctx.cwd["it"], {"TILO_IDLE_WAKEUP_WINDOW_SECS": str(ctx.options.wakeups_window)}, check=False)
    return adapters.idle_wakeups([log.read_text(encoding="utf-8", errors="replace")], ctx.env)


def run_fs(ctx: RunContext) -> list[record.Record]:
    log = ctx.dir / "fs-gate.log"
    call(["bash", ctx.repo / "tools/with-test-environment.sh", ctx.bins["performance_gate"], "--nocapture",
          "--test-threads=1"], log, ctx.cwd["performance_gate"], {"TILO_FS_PERF_GATE": "1"}, check=False)
    return adapters.fs_gate([log.read_text(encoding="utf-8", errors="replace")], ctx.env)


RELEASE_SHELL = ("cargo", "build", "--locked", "--release", "-p", "shell", "--bin", "shell",
                 "--features", "dev-overrides")

# The groups of perf-scheduled.yml that measure a build of the repository.
# `footprint` measures published artifacts, so it has no A/B.
GROUPS: dict[str, Group] = {group.name: group for group in (
    Group("hosting", "release", "none",
          (("cargo", "build", "--locked", "--release", "-p", "tilo-relay", "--example", "relay_load"),),
          {"relay_load": "target/release/examples/relay_load"}, {}, {}, run_hosting),
    Group("latency", "dev", "lavapipe",
          (("cargo", "build", "--locked", "-p", "shell", "--bin", "shell"),),
          {"shell": "target/debug/shell"}, {}, {}, run_latency),
    Group("agent", "release", "lavapipe", (RELEASE_SHELL,),
          {"shell": "target/release/shell"}, {}, {}, run_agent),
    Group("render", "release", "lavapipe",
          (("cargo", "build", "--locked", "--release", "-p", "ui-examples", "--example", "ui-perf"),
           ("cargo", "build", "--locked", "--release", "-p", "shell", "--example", "redraw_bench"),
           ("cargo", "build", "--locked", "--release", "-p", "tilo-app", "--features", "test-support",
            "--example", "framework-scaling")),
          {"ui-perf": "target/release/examples/ui-perf", "redraw_bench": "target/release/examples/redraw_bench",
           "framework-scaling": "target/release/examples/framework-scaling"},
          {}, {}, run_render,
          {"strip_pan_bench": ("crates/session/shell/examples/strip_pan_bench.rs",
                               ("cargo", "build", "--locked", "--release", "-p", "shell", "--example",
                                "strip_pan_bench"),
                               "target/release/examples/strip_pan_bench")}),
    Group("wakeups", "dev", "lavapipe", (),
          {"shell": "target/debug/shell"},
          {"it": (("cargo", "test", "--locked", "-p", "shell", "--test", "it", "--no-run"), "it")},
          {"shell": "target/debug/shell"}, run_wakeups),
    Group("fs", "dev", "none", (),
          {}, {"performance_gate": (("cargo", "test", "--locked", "-p", "tilo-fs", "--test", "performance_gate",
                                     "--no-run"), "performance_gate")},
          {}, run_fs),
)}


# --- orchestration ------------------------------------------------------------

def git(repo: Path, *args: str) -> str:
    result = subprocess.run(["git", *args], cwd=repo, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {result.stderr.strip()}")
    return result.stdout.strip()


def test_executable(command: tuple[str, ...], target: str, repo: Path, log: Path) -> tuple[Path, Path]:
    """Build a test target; return its executable and its package directory."""
    with log.open("ab") as stream:
        result = subprocess.run([*command, "--message-format=json"], cwd=repo, stdout=subprocess.PIPE,
                                stderr=stream, text=True, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"{' '.join(command)} exited {result.returncode}; see {log}")
    found = []
    for line in result.stdout.splitlines():
        if not line.startswith("{"):
            continue
        message = json.loads(line)
        if (message.get("reason") == "compiler-artifact" and message.get("executable")
                and message.get("target", {}).get("name") == target and message.get("profile", {}).get("test")):
            found.append((Path(message["executable"]), Path(message["manifest_path"]).parent))
    if len(found) != 1:
        raise RuntimeError(f"{' '.join(command)}: expected one test executable named {target}, found {len(found)}")
    return found[0]


def build_side(groups: list[Group], side: str, commit: str, repo: Path, out: Path) -> dict[str, Any]:
    """Check out `commit`, build every group and keep its artifacts."""
    git(repo, "checkout", "--quiet", "--detach", commit)
    manifest: dict[str, Any] = {}
    for group in groups:
        directory = out / "bin" / side / group.name
        directory.mkdir(parents=True, exist_ok=True)
        log = out / "logs" / f"build-{side}-{group.name}.log"
        print(f"== build {side} {commit[:10]}: {group.name}", flush=True)
        for command in group.builds:
            call(list(command), log, repo)
        cwd = {}
        for name, (command, target) in group.tests.items():
            executable, package = test_executable(command, target, repo, log)
            shutil.copy2(executable, directory / name)
            cwd[name] = str(package)
        for name, path in group.binaries.items():
            shutil.copy2(repo / path, directory / name)
        for name, (source, command, path) in group.optional.items():
            if not (repo / source).is_file():
                print(f"== {side} {commit[:10]} has no {source}; {name} is not built", flush=True)
                continue
            call(list(command), log, repo)
            shutil.copy2(repo / path, directory / name)
        manifest[group.name] = {"cwd": cwd}
    return manifest


def side_env(options: argparse.Namespace, group: Group, commit: str) -> dict[str, Any]:
    # record.environment names the checkout's commit; a side names its own.
    env = {"commit": commit, "profile": group.profile, "renderer": group.renderer,
           "virtualization": options.virtualization, "runner": options.runner,
           "hardware": record.detect_hardware(), "started_at": record.utc_now()}
    record.from_samples("env.check", "count", [0], **env)
    return env


def run_once(group: Group, side: str, round_: int, sides: dict[str, dict[str, str]], repo: Path,
             out: Path, manifest: dict[str, Any], options: argparse.Namespace) -> list[record.Record]:
    directory = out / "runs" / group.name / f"{round_:02d}-{side}"
    directory.mkdir(parents=True, exist_ok=True)
    bins = {path.name: path for path in (out / "bin" / side / group.name).iterdir()}
    for name, path in group.stage.items():
        shutil.copy2(bins[name], repo / path)
    env = side_env(options, group, sides[side]["commit"])
    env["meta"] = {"ab_side": side, "ab_round": str(round_), "ab_ref": sides[side]["ref"],
                   "ab_harness": options.harness}
    env_json = directory / "env.json"
    env_json.write_text(json.dumps(env, sort_keys=True), encoding="utf-8")
    cwd = {name: Path(path) for name, path in manifest[side][group.name]["cwd"].items()}
    records = group.run(RunContext(repo, bins, cwd, directory, env, env_json, options))
    if not records:
        raise RuntimeError("the run produced no records")
    for item in records:
        # Harness scripts write their own meta; the pairing keys are ours.
        item.meta = {**item.meta, **env["meta"]}
        item.commit = sides[side]["commit"]
    return records


def order(round_: int) -> tuple[str, str]:
    """Balanced order: odd rounds base first, even rounds candidate first."""
    return SIDES if round_ % 2 == 1 else (SIDES[1], SIDES[0])


def run(options: argparse.Namespace, repo: Path, groups: list[Group]) -> int:
    out = options.out.resolve()
    if git(repo, "status", "--porcelain", "--untracked-files=no"):
        raise RuntimeError("the checkout has tracked changes; commit them first (A/B checks out both sides)")
    if options.rounds < min_rounds():
        raise RuntimeError(f"{options.rounds} rounds cannot reach {CONFIDENCE:.0%}; use at least {min_rounds()}")
    sides = {side: {"ref": ref, "commit": git(repo, "rev-parse", "--verify", f"{ref}^{{commit}}")}
             for side, ref in (("base", options.base), ("candidate", options.candidate))}
    harness = options.harness = git(repo, "rev-parse", "HEAD")
    branch = subprocess.run(["git", "symbolic-ref", "--quiet", "--short", "HEAD"], cwd=repo,
                            capture_output=True, text=True, check=False).stdout.strip()
    (out / "logs").mkdir(parents=True, exist_ok=True)
    manifest = {}
    try:
        for side in SIDES:
            manifest[side] = build_side(groups, side, sides[side]["commit"], repo, out)
    finally:
        git(repo, "checkout", "--quiet", *((branch,) if branch else ("--detach", harness)))
    records: list[record.Record] = []
    failures: list[str] = []
    for group in groups:
        for round_ in range(1, options.rounds + 1):
            for side in order(round_):
                print(f"== {group.name} round {round_}/{options.rounds}: {side}", flush=True)
                try:
                    records += run_once(group, side, round_, sides, repo, out, manifest, options)
                except (RuntimeError, ValueError, OSError, KeyError) as error:
                    failures.append(f"{group.name} round {round_} {side}: {error}".splitlines()[0])
                    print(f"{group.name} round {round_} {side} failed: {error}", file=sys.stderr, flush=True)
    (out / "records.jsonl").write_text("".join(item.to_json() + "\n" for item in records), encoding="utf-8")
    analysis, text = write_report(out, records, sides, [group.name for group in groups], harness, failures)
    print(text, end="")
    if options.summary:
        with options.summary.open("a", encoding="utf-8") as stream:
            stream.write(text)
    # A verdict is evidence, not a gate: only a run that could not measure fails.
    return 0 if analysis.paired and not failures else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    go = commands.add_parser("run", help="build both refs, run them interleaved and analyse")
    go.add_argument("--groups", required=True, help=f"comma-separated: {', '.join(GROUPS)}")
    go.add_argument("--base", default="origin/main")
    go.add_argument("--candidate", default="HEAD")
    go.add_argument("--rounds", type=int, default=8)
    go.add_argument("--out", type=Path, default=Path("target/perf-ab"))
    go.add_argument("--summary", type=Path, help="also append the summary here ($GITHUB_STEP_SUMMARY)")
    go.add_argument("--virtualization", default="none", choices=sorted(record.VIRTUALIZATIONS))
    go.add_argument("--runner", default="local", help="the runner class, as record.py env takes it")
    go.add_argument("--hosting-minutes", type=int, default=3, help="measured minutes per hosting run")
    go.add_argument("--hosting-relay-cpus", type=int, default=0,
                    help="confine the relay to this many CPUs, as on a VM with that many vCPUs (0: all)")
    go.add_argument("--wakeups-window", type=int, default=30, help="idle window per wakeups run, seconds")
    again = commands.add_parser("analyse", help="analyse the records of an earlier run again")
    again.add_argument("records", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "analyse":
            records = record.read(args.records)
            sides, groups, harness = provenance(records)
            print(summary(analyse(records), sides, groups, harness, []), end="")
            return 0
        names = [name for name in args.groups.split(",") if name]
        unknown = [name for name in names if name not in GROUPS]
        if unknown or not names:
            raise RuntimeError(f"unknown groups {unknown} (A/B groups: {', '.join(GROUPS)})")
        repo = Path(git(Path.cwd(), "rev-parse", "--show-toplevel"))
        return run(args, repo, [GROUPS[name] for name in names])
    except (RuntimeError, ValueError) as error:
        print(f"ab.py: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
