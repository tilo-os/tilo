# Runtime benchmark

Goal: measure real Photos/Word Processor action latency and live CPU frame costs
without introducing alternate production paths or platform-specific timing code.

- Reuse the signed cross-app proof as the setup and correctness check.
- Reuse `TILO_PROFILE_OUT` and `TILO_UI_PRESENT`; keep CPU timings distinct from GPU/display latency.
- Record monotonic action durations, bounded warmup, measurement phase boundaries,
  raw samples, build information and median/p95/p99.
- Test percentile units, missing evidence rejection, phase filtering and render-log parsing.
- Build and run the live benchmark; review and document measured results and limits.
