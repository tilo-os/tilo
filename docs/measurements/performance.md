# Performance results

Performance KPIs (what Tilo markets, and how each is measured; their targets are
the `source` lines of `tools/perf/kpi-budgets.toml`) are measured by the performance
run: `tools/ci/perf-scheduled.sh <group>` measures one group, and `tools/ci/perf-compare.sh`
compares every group's records and publishes them where the run holds the store's token.
No workflow runs it: it is not a required check and never runs per push. The records of
the groups that passed are compared (and published), and the groups that left no records
fail the run. A record the store already holds keeps the verdict it got and is not published again. Every result is
one JSON record (`tools/perf/record.py`): KPI id, unit, n, nearest-rank
p50/p95/p99/max, commit, build profile, renderer, virtualization, runner class
and hardware. `tools/perf/compare.py` compares each record only with stored
records of the same KPI and environment, and fails the run on

- a p50 more than 10 % or a p99 more than 20 % above the median of the last five
  comparable runs, on two consecutive runs (one bad night is reported as
  `regression-pending`). A noisy KPI sets `baseline_min_runs` (three for latency
  timing KPIs; one for dropped frames) and is compared once that many
  comparable runs, or one accepted run, are stored; until then a run is `new`
  and joins the baseline;
- an enforced ceiling in `tools/perf/kpi-budgets.toml` (non-enforced ceilings
  are marketing targets and are only reported);
- a selected benchmark that produced no record, reported as `missing` and
  counted apart from regressions. A run is owed only its own workflow's KPIs:
  `[jobs]` in `tools/perf/kpi-budgets.toml` names the workflow (`compare.py
  --job`) that produces each group, and a job may claim a single KPI of
  another job's group, as VM acceptance claims `render.frame_cpu_ms`. A
  budget no job produces does not load.

The records, each with its verdict, are appended to the public generic package
`tilo-perf-results` (one immutable file per run, a version per UTC month), so
the current numbers come from there, not from `docs/benchmarks/`. Read them with
`python3 tools/perf/store.py fetch https://code.tilo.so`. The groups are `fs`,
`footprint` (app and OS image sizes), `agent`, `render`, `search` and `server`;
`tools/ci/perf-scheduled.sh <group>` measures one from your tree. `search` runs the palette search's own measurement test, which fails over its budget and
writes no record. `agent` times what an agent's calls cost through the session's agent
action API; a whole app task is not this job's, because a builder starts no app. A group's numbers are compared with the store without
writing to it by `python3 tools/perf/compare.py --results <records.jsonl> --store
https://code.tilo.so --groups <group>` on the records it wrote to `/target/perf/<group>/`.
`compare.py`'s `--accept` takes this run's values as
the new baseline start after an intended change (from main only); and
`--synthetic` with `--synthetic-factor` multiplies a run's values to prove
the check. A 1.25 factor passes as pending in the first run of a series and fails
in the second. Synthetic records never enter a real baseline, and `fetch` leaves
them out (it says how many on stderr; `--synthetic` prints them too): their
values are multiplied on purpose and their `regression` verdicts are the check
test, not a measurement.

The installed desktop's groups, `boot`, `session`, `idle`, `memory`, `disk` and `app`, and
its one render KPI, `render.frame_cpu_ms` (compositor CPU per painted frame, R04), come from
the **VM acceptance** run instead (job `vm-acceptance`): it already boots a
dev disk in a KVM guest and opens a session on it, so its records
(`scripts/vm-acceptance/run.py --records`) are compared with the store and published
under the same rules, rather than the performance run booting a second VM. They name
`renderer=lavapipe` and `virtualization=kvm`: software-rendered guest
numbers, compared only with each other and never quoted as hardware figures.
`scripts/vm-acceptance/README.md` ("Performance records") lists the KPIs.

The `server` group measures the server's cost per member-hour
(`server_load`, workspaces of four): fan-out latency and the priced capacity of a server VM.

Input-to-presentation latency and idle wakeups of an installed desktop are
the **VM acceptance** run's (`window`, `idle` and `resources` checks), not this
workflow's: its `idle.wakeups_per_s.*` records are what hold an idle desktop to
its budget. Input latency on the reference machines is a weekly, report-only
run through `tilo-vm`, outside this workflow:
[docs/benchmarks/reference-hardware.md](../benchmarks/reference-hardware.md).

To add a benchmark (a KPI group):

1. Name its KPIs `<group>.<metric>[.<variant>]`, for example
   `render.frame_ms.4k` or `agent.invoke.local.noop`.
2. Make it produce records. Either the benchmark writes samples, one number per
   line, and calls
   `python3 tools/perf/record.py emit --kpi ... --unit ... --samples-file ... --env-json env.json`,
   or a function in `tools/perf/adapters.py` parses the output it already
   prints, as `fs-check` does for the tilo-fs check's `PERF` lines. Make
   `env.json` with `record.py env --profile ... --renderer ... --virtualization
   ... --runner ...`. State the environment truthfully: llvmpipe, TCG or a dev
   build is a different environment from GPU, KVM or release, and is never
   compared with them.
3. Add the group to `tools/ci/perf-scheduled.sh`, and the group to
   `[jobs.perf-scheduled]` in `tools/perf/kpi-budgets.toml`. If the group
   has an A/B (`tools/perf/ab.py` `GROUPS`), build the same benchmarks there.
   The check runs on the node class the KPI is defined for and consumes
   artifacts that are already built: a published tapp or image, or an example
   built once in that check. It must not start a full build. It writes its
   records to `/target/perf/<group>/*.jsonl`.
4. List every KPI the group must report in `tools/perf/kpi-budgets.toml`, with
   its unit, its ceilings and `enforce`, and override the tolerances only with a
   measured reason. A series the machine cannot hold to a number says
   `baseline_only = true` instead of a ceiling, and is compared with its own
   baseline alone. A listed KPI that stops reporting fails the run.
5. Run the group with `tools/ci/perf-scheduled.sh <group>` from your tree; only main writes to the store.

Pull requests may check only on deterministic, runner-independent counters
(bytes, allocation counts, raster pixels), only when the measured paths change,
and following `tools/web/wasm_size_ratchet.py`. Wall-clock KPIs belong in
this job.

## Proving a performance change (A/B)

A nightly record cannot prove what one change did to a wall-clock or CPU
KPI: it is compared with earlier nights on other hosts, and the shared
runners differ by more than most changes (the unchanged server bench has
measured 15.9 and 31.2 CPU-seconds per member-hour). To claim a gain,
or to show there is no regression, run an A/B from a clean checkout on Linux:

```bash
python3 tools/perf/ab.py run --groups server --base origin/main --candidate <commit> --rounds 8 --out target/perf-ab
```

For each group in `--groups` (`server`, `render`, `agent`, `fs`;
`footprint` is bytes of published artifacts and `search` is a test's own budget,
so neither has an A/B)
builds the base and the candidate in one checkout, one after the other, and
runs that group's existing benchmark on both, alternating, for `--rounds`
rounds on one node (`tools/perf/ab.py`). Odd rounds run the base first and
even rounds the candidate first, so drift over the job falls on both sides.
Each round pairs the two runs, so a slow host slows both and cancels out.
The binaries come from each side's commit. The harness (`scripts/perf/`, the
adapters) comes from the checked-out commit and drives both sides.

For each KPI, the summary (`summary.md`) judges the p50 and the p99. The estimate is the
paired ratio candidate/base (Hodges-Lehmann), with its distribution-free
95 % interval (exact Wilcoxon signed rank). Beside it are the rounds the
candidate won and lost, and the sign test. The verdict:

- **improved** or **regressed**: the whole interval lies on one side of no
  change (lower is better, except fps);
- **unchanged**: both sides measured the same in every round;
- **inconclusive**: the interval includes no change. Six rounds is the
  minimum for a 95 % interval; then every round must agree. Add rounds
  rather than rerunning until a run agrees with you.

A KPI that only one side reports, such as a metric the candidate adds, is
listed and not paired. Paste the summary table into the change's description.
Rerun the analysis with
`python3 tools/perf/ab.py analyse records.jsonl`. A/B results never enter the
store or the baseline, and the job fails only when a run could not measure,
never on a verdict.
