# UX evidence implementation plan

**Goal:** Deterministic usability evidence plus unscripted API-agent trials of the real application.
**Spec:** `docs/superpowers/specs/2026-09-16-ux-evidence-design.md`
**Architecture:** A persistent Rust host exposes bounded visible inputs and captures. Python owns task oracles, deterministic probes, API exploration and reports.
**Tech:** Existing Rust Driver and native shell, Python standard library, OpenAI Responses API with screenshot inputs and strict structured actions.

## Global constraints
Follow the spec's binding requirements. Preserve unrelated changes and credentials. No automatic commits. Agent success claims are never the oracle.

## Task 1: Persistent host
- [x] Add `--interactive` to ux-bench, consuming existing --scenario configuration and --out new directory; scenario routes are not executed in this mode.
- [x] Implement JSONL contract in spec with one Driver/session; initial response is observe, commands emit exactly one response, finish saves state.
- [x] Write negative protocol tests and persistent click/key/state/capture integration test. Build and document actual contract in task report.

## Task 2: Deterministic and agent evaluator
- [x] Add Python host transport with deadlines, strict schema validation and artifact retention.
- [x] Implement private exact-state/text oracles and evidence checks, independent of model claims. Cover no-ops, recovery, route budget, empty evidence and false-completion negatives.
- [x] Add screenshot-only API agent with bounded structured actions, repeated trials, exact token/latency accounting and HTML/JSON report. Unit test using fake transport; then run real API trials.

## Task 3: Integration and evidence
- [x] Run deterministic usability scenarios and existing regression checks.
- [x] Run live agent attempts for all specified tasks and variants; preserve every failure and classify without weakening requirements.
- [x] Independently review implementation, inspect rendered captures and final report; document coverage and limits.

Parallel ownership: Task1 owns shell/src/bin/ux-bench.rs and its new Rust support module only. Task2 owns new scripts/ux-bench usability Python files only. Controller owns spec, plan, integration evidence and docs. The fixed JSONL interface permits independent implementation.
