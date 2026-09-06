All nine declared validation checks passed: format/diff; strict production and
vendor-test Clippy; test-inclusive changed-line/compiler diagnostics zero;
118 vendor tests, 216 DRM library tests, 53 benchmark unit tests and two native
pixel oracles. Nine pre-existing ignored tests were not executed in these suites.
All 15 new pure tests executed and passed. All nine raw logs and 37 source/config
hashes were audited. No runtime shader/rendering algorithm changed.

Ordinary shell-only locked release build is next. The six diagnostic probes
remain unmeasured and cannot count as performance acceptance. The earlier
independent dense-oracle instability is not declared resolved by these passes.
