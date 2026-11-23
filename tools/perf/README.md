# perf: the session's performance, measured and held

What a person notices first is how long the session takes to be usable and
whether it stays smooth while it animates. Both are measured here, per device
class, and checked against budgets in `kpi-budgets.toml`.

| Harness | Measures | Runs on |
|---|---|---|
| `tools/perf/startup_marks.py` | a login's own startup timeline, from the session's marks to `first-frame-presented` | the device's session (`TILO_STARTUP_METRICS_PATH=<file>`) |
| `tools/perf/check_kpi.py` | compares any of those measurements with its budget | CI, a person |

Budgets are per device class, because the same millisecond is not the same
promise on the OPi 3B as on a laptop with a real GPU:

- `opi3b` — the floor: four Cortex-A76 cores, no GPU worth the name.
- `x86` — a desktop or laptop x86 machine with a real GPU.

## Measuring

```sh
# A login on the device itself.
TILO_STARTUP_METRICS_PATH=~/session-startup.json tilo-session
tools/perf/startup_marks.py ~/session-startup.json --device opi3b --json opi3b-startup.json

# Check every measurement against its budget.
tools/perf/check_kpi.py check opi3b-startup.json
```

The first frame is a wall-clock promise, so it is measured with wall clock.
Frame time is measured through the production five passes (`sync`,
`build_layout`, `apply_layout`, `draw`, `handle_event`) with no GPU: it is the
CPU a frame owes, which is what a slow machine runs out of.

## Budgets

`kpi-budgets.toml` holds one table per device class and measurement kind. A
budget follows a measurement: set it from a run on the class, with the
headroom noted in its comment, and lower it when the measurement improves.
`test_check_kpi.py` proves the check fails when a budget is exceeded.

Per-frame allocations are budgeted from the same run (`steady.allocs_per_frame`): a
frame that allocates more than it did is a frame that will stutter when the
allocator does something other than hand back a cached block.
