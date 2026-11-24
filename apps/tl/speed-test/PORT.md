# Speed Test, ported

The Rust `apps/speed-test` is gone; this package is the app, under the same id
(`so.tilo.speed-test`). Every row below is a feature of the Rust app, where it lives now, and the
test that walks it.

Measured facts are a `Measurement` record in the workspace's `SpeedTest` root, newest first; a run
in flight is a `Run` record beside them, so a person's dial and the run behind it survive closing
the window and are the same on every device of the workspace.

## Actions

| Rust | Now | Test |
|---|---|---|
| `record` (external): ping, jitter, download, upload, server, device; keeps the newest 20 | `action record` on `SpeedTest` | `e2e_speed_test::a_measurement_is_recorded_newest_first_and_the_twentieth_lets_the_oldest_go` |
| `clear` (destructive): forgets every measurement | `destructive action clear` | `e2e_speed_test::clearing_forgets_every_measurement_and_the_dial_starts_over` |
| `SpeedEffect::Start` (job, cancellable, own device; offered as "Run speed test"): six latency requests, a 10 MB streamed download, a 2 MB upload, the server from `cf-meta-colo`, "Data used" as both transfers | `primary action start` over `engine.start("run")`, followed by `poll`; the measuring is `crates/engines/speed-engine` | `e2e_speed_test::a_run_follows_the_device_and_keeps_what_it_measured`, `speed_engine::tests::*` |
| `SpeedEffect::Stop` | `action stop` over `engine.cancel` | `e2e_speed_test::stopping_ends_the_run_and_the_dial_goes_back_to_ready` |
| Start unavailable while a test runs; Stop unavailable when none does | `applies` on both actions | `e2e_speed_test::a_running_test_offers_stop_and_an_idle_one_run` |
| A run that cannot reach the server fails its stage, never times the error page | `speed-engine` (refusals fail the stage) | `speed_engine::tests::a_server_that_refuses_fails_the_stage_instead_of_timing_its_refusal` |
| 12 s timeout, user agent `Tilo Speed Test/0.1` | `speed-engine` | as above (through the client it builds) |
| The device that ran it is kept with the measurement | `device` from the engine's `host_name` | `e2e_speed_test::a_run_follows_the_device_and_keeps_what_it_measured` |
| History sentences ("Measured … down and … up from …", "Cleared the measurements") | the actions' own `label`/`did`, which the platform's history reads, with the measurement's values in the change | `e2e_speed_test::history_says_what_a_run_measured` |

## The window

| Rust | Now | Test |
|---|---|---|
| The dial: a large progress ring, its readout and unit per phase, an arc bounded and rising with the speed, eased every frame | `ui.ProgressRing` with `arc_of`/`readout_of`/`unit_of`; the ring's own motion glides between readings | `e2e_speed_test::the_dial_reads_the_stage_it_is_in` |
| Status line: Ready to test, Measuring latency/download/upload, Test complete, Ready to try again | `status_of` | `e2e_speed_test::the_dial_reads_the_stage_it_is_in` |
| One action: Run speed test (primary) or Stop test (secondary) | `button(start)`, `button(stop)`, from the actions' own roles | `e2e_speed_test::a_running_test_offers_stop_and_an_idle_one_run` |
| Stepper Ping/Download/Upload, its stage; none before a run or after a failure | `ui.Stepper` with `stage_of` | `e2e_speed_test::the_stages_follow_the_run` |
| Results tiles: Ping (ms), Download (Mbps), Upload (Mbps) | `ui.StatGroup`/`ui.StatTile` in the results column | `e2e_speed_test::a_finished_run_shows_its_numbers` |
| Connection: Server, Jitter, Data used | `ui.PropertyList` | `e2e_speed_test::a_finished_run_shows_its_numbers` |
| Connection quality: Video calls, 4K streaming, Cloud gaming, each Excellent/Good/Limited with a status tone | `quality_row` and `ui.Badge` | `e2e_speed_test::the_connection_quality_follows_the_numbers` |
| Recent tests: down, up and ping, how long ago, newest first, the one on the dial left out | `recent_row` with `ui.ListRow` and `ui.Time` | `e2e_speed_test::recent_tests_read_newest_first_without_the_one_on_the_dial` |
| "Test interrupted" banner with the stage's words | `ui.Banner(kind: .error)` | `e2e_speed_test::a_failed_run_says_why` |
| An idle window centred, the server named once | the ready page: the page's stack between two spacers | `e2e_speed_test::the_ready_page_centres_the_test_and_names_the_server_once`, `tilolang layout-check` |
| Results beside the test, stacked below it when the window is narrow | `split(collapse: .stack)` | `tilolang layout-check` |
| Scenarios for shots: idle, running, complete, error, slow, narrow, short | the scenarios in this file; the sizes are `tilolang layout-check`'s | `tilolang layout-check` |
| German window | `locales/de-DE.ftl` | `policy/catalogue-coverage` |
| Search words, app name, tagline, icon | the app header | `policy/catalogue-coverage`, the app surface's checks |
| Network: only `speed.cloudflare.com` | the header's `web:`; the measuring engine reaches it | `speed-engine` |

## Not a row, on purpose

`apps/README.md` described region selection; no Rust code or copy for it existed, so there is
nothing to port.
