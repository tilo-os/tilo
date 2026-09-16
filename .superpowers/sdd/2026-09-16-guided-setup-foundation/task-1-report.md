# Task 1 report: capability-driven setup model

## Changes

- Added the pure setup decision model with capability-gated Desktop and Full OS
  support.
- Added folder modes, storage accounting with checked arithmetic, and structured
  review errors carrying folder indices and path roles where relevant.
- Added lexical validation for absolute paths, parent traversal, duplicate and
  overlapping active selections, and component-boundary comparisons.
- Exported `setup::model` from the Imager library. No UI, filesystem mutation,
  disk operation, or privileged authorization was added.

## TDD evidence

- RED: `rustc --edition=2024 --test apps/imager/src/setup/model.rs -o
  /tmp/qos-guided-setup-model-tests` exited 1 because the public model types and
  behavior did not yet exist.
- GREEN: the same standalone compile followed by
  `/tmp/qos-guided-setup-model-tests` passed 16 tests.
- Integration: `cargo test -p imager --lib setup::model` passed 16 tests with
  133 unrelated tests filtered out.
- Full regression: `cargo test -p imager --lib --no-default-features` passed all
  149 tests.

## Limitations

- Path checks are lexical. Platform adapters must still canonicalize paths,
  compare volume identity, apply filesystem case-folding rules, and revalidate
  access and mount safety immediately before I/O.
- Capability booleans are facts supplied by future native adapters; this task
  does not probe platforms or enable an installation route.
- Connected mode is modeled for review only. Synchronization, boot staging,
  migration, and disk mutation remain future components.

## Commit

`feat(imager): model guided setup choices and review`
