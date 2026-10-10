# Browser dictation composition lifecycle review

Two private-world lifecycle defects blocked safe dictation recovery: a synthetic composition end on an unrelated field cleared the original field's state, and a removed custom editor left the consumer permanently composing. The observer now tracks synthetic targets independently and prunes disconnected targets; the consumer relies on that mandatory observer. Every composition start still retires the original lease permanently. Trusted native composition can clear only through a trusted end event. Unknown observation and excessive synthetic targets fail closed.

The [DOM connectivity definition](https://dom.spec.whatwg.org/#dom-node-isconnected) supplies the native connectivity getter. [UI Events](https://w3c.github.io/uievents/#event-type-compositionend) defines the composition-end event. Synthetic editor removal does not establish that a trusted native composition ended.

The hermetic fixture extracts and executes the actual observer from `host.cc` and the actual consumer helpers/listeners from `page_automation.js`. It does not simulate Blink selection, native editing history, CSS, origins or microphone capture. Nine cases cover unrelated and overlapping targets, disconnected successors, trusted-event isolation, root replacement, bounded observation, consumer recovery, missing observation and protected fields. The automatic `tools/tests` wrapper runs this fixture and fails if its subject or Node runtime is unavailable.

Causal receipts and source SHA-256 values are in `dictation-browser-composition-review.json`; logs are under `tools/release/fixtures/dictation_browser_composition_20261010/`:

- Selected baseline `e0cf92a9482dcddcd131cab6076cacdf5b42c43b`: 4 pass, 5 fail.
- New observer with baseline consumer: 8 pass, 1 fail.
- Complete candidate: 9 pass, 0 fail, no skips.

Two actual CEF regression journeys were added: unrelated custom composition end, and removal with stale final/cancel followed by a fresh selected replacement and native undo/redo. Their Python syntax passed; they have **not been executed**. JavaScript syntax and the Python unit wrapper passed. No local Cargo build, new runtime/model acquisition, microphone permission, deployment or merge occurred.

Native qualification remains blocked by loss of the known approved CEF SDK/runtime/build artifacts with the owned builder worktree. Restore those artifacts or provide the location of an existing approved matching runtime and blocker; do not implicitly download a replacement SDK. C++ compilation and actual Chromium composition/history regressions remain required before integration. Keep the PR draft with `hold`.
