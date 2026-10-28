# Script map

Repository scripts are grouped by the workflow they drive. Keep reusable build,
policy, and release implementation under `tools/`; this directory is for
operator-facing development and validation flows.

| Directory | Purpose |
|---|---|
| `headless-examples/` | Focused compositor and collaboration proof drivers |
| `perf/` | Performance measurement, PGO, BOLT, and boot timing |
| `tests/` | Unit tests for script implementations |
| `ux-bench/` | Declarative user-flow benchmarks and their runner |

The few root scripts are cross-cutting repository checks or entrypoints that do
not belong to one workflow. New multi-file workflows should get a named
subdirectory rather than another filename prefix.
