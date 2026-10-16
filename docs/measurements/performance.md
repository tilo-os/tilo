# Performance results

Performance KPIs (what Tilo markets, and how each is measured; their targets are
the `source` lines of `tools/perf/kpi-budgets.toml`) are measured by **Perf / scheduled**
(`.forgejo/workflows/perf-scheduled.yml`), nightly at 01:15 UTC and by manual
dispatch. It is not a required check and never runs per push. Every result is
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
  `[lanes]` in `tools/perf/kpi-budgets.toml` names the workflow (`compare.py
  --lane`) that produces each group, and a lane may claim a single KPI of
  another lane's group, as VM acceptance claims `render.frame_cpu_ms`. A
  budget no lane produces does not load, and `tools/perf/test_compare.py`
  holds each workflow's selectable groups equal to its lane.

The records, each with its verdict, are appended to the public generic package
`tilo-perf-results` (one immutable file per run, a version per UTC month), so
the current numbers come from there, not from `docs/benchmarks/`. Read them with
`python3 tools/perf/store.py fetch https://code.tilo.so`. Dispatch inputs:
`kpis` selects groups (`fs`, `footprint` (app and OS image sizes), `agent`, `latency`, `render`, `wakeups`, `search`, `hosting`, `browser`, `stream`); `mode: ab` with `ab_base`,
`ab_candidate` and `ab_rounds` runs the A/B lane below instead; `accept` takes this run's values as
the new baseline start after an intended change (from main only); and
`synthetic_series` with `synthetic_factor` multiplies a run's values to prove
the gate. A 1.25 factor passes as pending in the first run of a series and fails
in the second. Synthetic records never enter a real baseline, and `fetch` leaves
them out (it says how many on stderr; `--synthetic` prints them too): their
values are multiplied on purpose and their `regression` verdicts are the gate
test, not a measurement.

The installed desktop's groups, `boot`, `session`, `idle`, `memory`, `disk` and `app`, and
its one render KPI, `render.frame_cpu_ms` (compositor CPU per painted frame, R04), come from
**Dev / VM acceptance** (`vm-acceptance.yml`, lane `vm-acceptance`) instead: its nightly run already boots the
newest published dev disk in a KVM guest and opens a session on it, so its `perf` job
compares that run's records (`scripts/vm-acceptance/run.py --records`) with the store
and publishes them, under the same rules, rather than Perf / scheduled booting a second
VM. They name `renderer=lavapipe`, `virtualization=kvm` and the runner class
`runner-disk-100g`: software-rendered guest numbers, compared only with each other and
never quoted as hardware figures. `scripts/vm-acceptance/README.md` ("Performance
records") lists the KPIs.

The `browser` group qualifies the browser runtime: the release desktop bundle
(`build.sh --bundle desktop`, the bytes t's `image-web-entry` publishes) in headless
Chrome through `scripts/perf/browser_qualification.mjs`, which first proves the
page ran exactly that commit's clean bundle on the WebGPU adapter the records
name, then measures load, first opens, window activation, the demo workload's
frame intervals and per-frame runtime CPU, and an idle desktop, and reports them
against the runtime's goals (30 fps minimum and 120 fps for the demo, 50 ms
activation). The runner has no GPU, and Chrome gives WebGPU no CPU driver but
its own SwiftShader, so the records name `renderer=swiftshader`. SwiftShader
takes tens of seconds over a panned FHD frame there, so the nightly run leaves
the demo out (`--demo-seconds 0`); its frame rate and goals are judged where a
GPU draws.
A hardware qualification is the same harness on the machine under test, quiet
and with the goals enforced:

```bash
crates/browser/tilo-browser-runtime/scripts/build.sh "$PWD/target/qualify" --bundle desktop
node scripts/perf/browser_qualification.mjs target/qualify --out qualify.json \
  --renderer gpu --expect-revision "$(git rev-parse HEAD)" --max-load 0.25 --enforce-goals
```

`--cpu-throttle 4` slows the page's CPU fourfold, a stand-in for older hardware
on a fast machine; such a run is never this group's series.

The `hosting` group has two jobs: the relay's cost per collaborator-hour
(`relay_load`, rooms of four) and a public room host at 100 guests
(`hosting-room`: `tilo-room-load bench --guests 100`, three runs on `runner-large`, through
`adapters.py room-load`), which guards the guests' p99 join and chat delivery
and the host's peak memory and CPU. The capacity curve behind that point and
the deployed caps (guests and spectators from 50 to 4000, the caps at once, and
the price of 100 concurrent guests per node) is **Perf / room host capacity**
(`perf-room-capacity.yml`, manual dispatch, `tools/perf/room_capacity.py`);
[docs/benchmarks/room-host-capacity-2026-10.md](../benchmarks/room-host-capacity-2026-10.md)
records a run.

The `stream` group qualifies a native window on the browser desktop:
`scripts/perf/native_stream_qualification.mjs` runs this commit's host
(`tilo-relay dev`), a session (tilo-relay's `legacy_stream_source` example: the
shell's legacy-share protocol and production H.264 encoder over a drawn window
whose every frame carries its number) and the release desktop bundle in
headless Chrome, which decodes the stream with WebCodecs. It reads back every
picture the page draws, so it measures startup to the first picture, capture
to picture latency, the frame rate, the frames never drawn and click to session
input latency, and fails any run that shows a corrupt, misnamed or older
picture, a decoder error, a wrong last frame or a lost press. The runner has no
GPU, so the nightly numbers are a software encode (30 fps), a software decode
and a SwiftShader desktop; a hardware qualification adds a hardware encoder,
decoder and GPU desktop, with the goals enforced:

```bash
cargo build --release -p tilo-relay --features web,stream-source --bin tilo-relay --example legacy_stream_source
node scripts/perf/native_stream_qualification.mjs target/qualify --out stream.json \
  --relay target/release/tilo-relay --source target/release/examples/legacy_stream_source \
  --renderer gpu --expect-revision "$(git rev-parse HEAD)" --max-load 0.25 --enforce-goals
```

The nightly runs the qualification twice in one job: H.264, then `--codec vp9`,
the software VP9 a session with no H.264 encoder (a stock device) streams. The
VP9 run is its own series, with the codec after the group in each KPI id
(`stream.vp9.frame.latency`), so neither codec's baseline judges the other's.

`--carrier https` takes the live plane over the HTTPS stream fallback instead of
WebTransport.

After the timed run, on the same host and in the same room (one the host
governs, as a shared workspace is), the harness drives the acceptance cases of
`scripts/perf/native_stream_acceptance.mjs`, and a case that breaks fails the
run like any other incorrect stream: focus moving onto and off the window with
clicks, a paste with Control+V, an input method's composition (only its
committed text reaches the window, once), the page resized and the window
resizing itself while it streams, a second device of the owner on a network
where WebTransport never becomes ready (it must take the HTTPS stream and drive
the window), a second person invited as a viewer (frames, but no control, and
input they send the host directly refused), the live plane dropped under the
page while a window streams (it must resume and end on the last frame), and
typing that never reaches a window that does not hold the keyboard or has
closed. Safari is not automated: WebKit's Linux builds (WebKitGTK and WPE,
which Playwright's WebKit is) are not Safari and have no WebTransport, and
Safari runs only on macOS, for a logged-in user who has enabled remote
automation (`safaridriver --enable`), which no CI runner here provides.

The `latency` group is the compositor's side only (headless, no panel).
Input-to-presentation latency on the reference machines is a weekly,
report-only run through `tilo-vm`, outside this workflow:
[docs/benchmarks/reference-hardware.md](../benchmarks/reference-hardware.md).

To add a benchmark (a KPI group):

1. Name its KPIs `<group>.<metric>[.<variant>]`, for example
   `render.frame_ms.4k` or `agent.invoke.local.noop`.
2. Make it produce records. Either the benchmark writes samples, one number per
   line, and calls
   `python3 tools/perf/record.py emit --kpi ... --unit ... --samples-file ... --env-json env.json`,
   or a function in `tools/perf/adapters.py` parses the output it already
   prints, as `fs-gate` does for the tilo-fs gate's `PERF` lines. Make
   `env.json` with `record.py env --profile ... --renderer ... --virtualization
   ... --runner ...`. State the environment truthfully: llvmpipe, TCG or a dev
   build is a different environment from GPU, KVM or release, and is never
   compared with them.
3. Add a job to `perf-scheduled.yml`, and add the group to `known` in its
   `plan` job, to the `compare` job's `needs` and to its job-result loop, and
   to `[lanes.perf-scheduled]` in `tools/perf/kpi-budgets.toml`. If the group
   has an A/B (`tools/perf/ab.py` `GROUPS`), build the same benchmarks there.
   The job runs on the runner class the KPI is defined for and consumes
   artifacts that are already built: a published tapp or image, or an example
   built once in that job. It must not start a full build. It uploads its
   records as the artifact `perf-<group>` (`*.jsonl`).
4. List every KPI the group must report in `tools/perf/kpi-budgets.toml`, with
   its unit, its ceilings and `enforce`, and override the tolerances only with a
   measured reason. A listed KPI that stops reporting fails the run.
5. Add tests for an adapter to `tools/perf/test_adapters.py`. Dispatch the
   workflow with `kpis=<group>` from your branch: it compares without writing to
   the store.

Pull requests may gate only on deterministic, runner-independent counters
(bytes, allocation counts, raster pixels), only when the measured paths change,
and following `tools/web/wasm_size_ratchet.py`. Wall-clock KPIs belong in
this lane.

## Proving a performance change (A/B)

A nightly record cannot prove what one change did to a wall-clock or CPU
KPI: it is compared with earlier nights on other hosts, and the shared
runners differ by more than most changes (the unchanged relay bench has
measured 15.9 and 31.2 CPU-seconds per collaborator-hour). To claim a gain,
or to show there is no regression, dispatch **Perf / scheduled** with
`mode: ab`:

```bash
fgj workflow run perf-scheduled.yml -R tilo/tilo --hostname code.tilo.so \
  --ref <your-branch> -f mode=ab -f kpis=hosting
# optional: -f ab_base=<ref> (default origin/main)
#           -f ab_candidate=<ref> (default the dispatched commit) -f ab_rounds=8
```

One job per group in `kpis` (`hosting`, `latency`, `render`, `agent`,
`wakeups`, `fs`, `search`, `browser`; `footprint` is bytes of published artifacts and has no A/B, nor has `stream` yet)
builds the base and the candidate in one checkout, one after the other, and
runs that group's existing benchmark on both, alternating, for `ab_rounds`
rounds on one runner (`tools/perf/ab.py`). Odd rounds run the base first and
even rounds the candidate first, so drift over the job falls on both sides.
Each round pairs the two runs, so a slow host slows both and cancels out.
The binaries come from each side's commit. The harness (`scripts/perf/`, the
adapters) comes from the dispatched commit and drives both sides.

For each KPI, the job summary judges the p50 and the p99. The estimate is the
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
listed and not paired. Paste the summary table into the pull request, and
link the run. The artifact `perf-ab-<group>` holds `ab-result.json`, every
record and every run's log. Rerun the analysis with
`python3 tools/perf/ab.py analyse records.jsonl`. A/B results never enter the
store or the baseline, and the job fails only when a run could not measure,
never on a verdict. The same tool runs locally on Linux, on a clean checkout:
`python3 tools/perf/ab.py run --groups hosting --base origin/main --candidate HEAD`.
