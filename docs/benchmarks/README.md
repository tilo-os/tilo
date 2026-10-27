# Benchmark investigations

Each `*.md` file here, with the `*.json` beside it, is a historical
investigation. It records a dated measurement at a named commit on named
hardware, and why someone made it. Those numbers describe that tree and that
machine, and they go stale with the next merge. They are not the current value
of any KPI, and nothing compares against them.

Current values come from the scheduled performance lane
(t's `perf-*` checks, tools/ci/perf-scheduled.sh). It writes one record per KPI per run
to the public generic package `tilo-perf-results`, each record with its verdict
against the rolling baseline:

```sh
python3 tools/perf/store.py fetch https://code.tilo.so
```

`fetch` leaves out the records of a gate test (`meta.synthetic_series`), whose
values are multiplied on purpose; `--synthetic` prints them too.

[Performance results](../measurements/performance.md)
explains the record format and the regression rule, and how to
add a benchmark. If you add an investigation here, keep its raw output in
the JSON file. Once its benchmark is in the lane, cite the lane's records
instead of copying their numbers.

`reference-hardware.md` is not an investigation: it names the machines and the
two lanes behind the latency KPIs, and how to run the weekly hardware one.
