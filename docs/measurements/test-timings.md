# Test time per package

How long each package's tests take, recorded by every whole run of `t`'s `test`
family, and a warning when one gets slower.

## Where the numbers come from

The family's wrapper (`tools/ci/t-nextest.sh`) runs the test binaries with
libtest's own JSON events on (`--message-format json-pretty`). Each event names
one test — the binary it belongs to before the first `::` — and how long it took.
`tools/ci/ci_test_timings.py record` reads them as they stream past, passes every
line on to the job's log exactly as libtest wrote it, and leaves one record per
package in the job's build directory, `target/test-timings.json` beside the
binaries it tested. A run's jobs add to the one file, so it holds the whole run:

```json
{
  "finished": true,
  "packages": {
    "shell": {"tests": 1420, "wall_seconds": 6480.4, "test_seconds": 6480.4,
              "slowest": [{"test": "e2e_shell_flows$opens_a_workspace", "seconds": 12.3}]}
  }
}
```

The time is the sum of each test's own `exec_time`, which starts at the test
function and leaves out starting its binary. It is not a job's wall time (t
reports that) and it is not a build time.

`summarise` reads the run's file and prints its table:

```
$ python3 tools/ci/ci_test_timings.py summarise --json target/test-timings.json
2 packages, 3 tests, 41.0s of test time
package                                tests      time   slowest  slowest test
shell                                      1     40.0s     40.0s  opens
tilo-fs                                    2      1.0s      0.6s  restores
```

## The baseline

`docs/measurements/test-timings.json` holds the committed baseline, written from
a complete run:

```bash
t test test                                  # or a package: t test test/shell
python3 tools/ci/ci_test_timings.py write-baseline \
    --json target/test-timings.json --baseline docs/measurements/test-timings.json --force
```

`write-baseline` refuses a run that did not finish its tests (nextest cancelled
it, or hit the hang bound), so a broken run cannot become the baseline. It also
leaves an existing baseline alone unless `--force`: moving it is deliberate, and
what moved it belongs in that commit.

Only the workspace's own packages are recorded. A run of an external crate's
tests (an upstream build job) passes `--third-party`, and the baseline keeps none
of them.

## The warning

`summarise` compares the run with the baseline and warns about each package at
least two seconds and 20% slower:

```
::warning title=Test time regression::shell took 2m10.1s against a baseline of 1m48.2s: 20% slower
```

`summarise` never fails the job it is asked about: the tests' verdict stays
nextest's, and the wrapper adds no verdict of its own. The two-second
floor keeps noise out — run-to-run noise on a busy machine is ±20% (see
[build speed](build-speed.md)), so a package that small can double without
meaning anything.

`--tolerance` changes the fraction, and
`python3 tools/ci/ci_test_timings.py compare` runs the comparison alone. The
baseline it compares against is a file, so nothing here is tied to one runner:
`compare` answers the same on a checkout, in CI, before a push and after one.

## Limits

A job's part of the run is partial whenever its run is: a package tested with a
test filter, an explicit test binary or retries is not recorded, and neither is a
run that stopped at its first failure (the wrapper adds `--no-fail-fast` so later
packages still run).

A package whose tests are split across jobs and platforms is recorded once per
job, so its time is the sum of them, and the baseline is whatever the run that
wrote it measured.

`t` keeps a job's log up to nextest's own summary, so the wrapper's table is not
in `t log`, and `t` runs a job on whichever node it schedules, so the run's file
is in that node's build directory rather than one a checkout shares. The records
are the job's own output; `summarise`, `compare` and `write-baseline` read them
wherever they are, which is why they take `--json` as a path. A regression
therefore shows where the records are read, not in the log beside the test
results.
