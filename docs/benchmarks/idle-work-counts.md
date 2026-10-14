# Deterministic app-idle scheduling coverage

The production idle loop already parks answered widget deadlines and does not
paint merely because a poll was due. This change preserves that behavior while
moving the process-tree timing measurement to an explicit scheduled executable.

The regression tests exercise the production `NativeAppTick` scheduling seam
with a real `FrameBuilder` and two adversarial widgets. Each widget retains its
poll deadline until draw consumes it. Counts come from the widgets' actual
update, apply-layout and draw callbacks, not inferred timing or CPU use.

## Repeated checks

Base: `a2f12b93b3`. A detached baseline worktree received only the scheduling
extraction, fixture constructor and regression tests; no benchmark migration.
Both that baseline and the candidate ran this command six times:

```sh
cargo test -p shell --lib idle_work_tests -- --nocapture
```

Each case contains 64 identical event sequences. All six runs passed on both
sides, giving 384 matching observations of each sequence per side:

| Sequence | Updates per app | Layouts per app | Draws per app |
| --- | ---: | ---: | ---: |
| Already-due poll, then park | 1 | 0 | 0 |
| Initial event, future poll, then park | 2 | 0 | 0 |
| Subsequent app change after the due-poll sequence | 1 additional | 1 additional | 1 additional |

Virtual instants are scheduler inputs. There are no sleeps, elapsed-time
assertions, retries or CPU budgets in these regression tests. The 128-event cap
fails a spin with its actual callback counts instead of waiting for a timeout.

## Negative controls

Restoring the original expired-deadline behavior (returning an answered deadline
unchanged instead of filtering it) failed both cases at the event cap. Both apps
performed **128 updates, zero layouts and zero draws**, rather than parking.

Restoring deadline-implies-paint (`pump_changed || deadline_due`) also failed both
cases: the due-poll sequence performed **1 update, 1 layout and 1 draw**, where
**1, 0, 0** was required; the future-poll sequence performed **2, 1, 1**, where
**2, 0, 0** was required. Both injected mutations were removed before the six
candidate runs.

## Scheduled measurement

The explicit `idle-wakeups` example preserves all five original budget assertions
and observation checks. It compiled and ran in the isolated native builder
environment with the built production shell binary. Its ten-second smoke sample
reported zero wakeups, zero loop ticks, zero CPU percent and no new threads
(`window_ms=10004`). The scheduled workflow still takes three 60-second samples.
The performance adapter and A/B tooling suites passed 90 focused Python tests.

These work-count tests cover app polling and the native app scheduling seam with
an unlocked desktop and available presenter capacity. They do not claim to count
every background service or exercise the entire native loop; the process-tree
measurement retains that broader observational role outside pull-request gates.
