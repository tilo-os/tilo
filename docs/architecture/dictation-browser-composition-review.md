# Browser dictation composition lifecycle review

Two private-world lifecycle defects blocked safe dictation recovery: a synthetic composition end on an unrelated field cleared the original field's state, and a removed custom editor left the consumer permanently composing. The observer now tracks synthetic targets independently and prunes disconnected targets; the consumer relies on that mandatory observer. Every composition start still retires the original lease permanently. Trusted native composition can clear only through a trusted end event. Unknown observation and excessive synthetic targets fail closed.

The [DOM connectivity definition](https://dom.spec.whatwg.org/#dom-node-isconnected) supplies the native connectivity getter. [UI Events](https://w3c.github.io/uievents/#event-type-compositionend) defines the composition-end event. Synthetic editor removal does not establish that a trusted native composition ended.

The hermetic fixture extracts and executes the actual observer from `host.cc` and the actual consumer helpers/listeners from `page_automation.js`. It does not simulate Blink selection, native editing history, CSS, origins or microphone capture. Nine cases cover unrelated and overlapping targets, disconnected successors, trusted-event isolation, root replacement, bounded observation, consumer recovery, missing observation and protected fields. The automatic `tools/tests` wrapper runs this fixture and fails if its subject or Node runtime is unavailable.

Causal receipts and source SHA-256 values are in `dictation-browser-composition-review.json`; logs are under `tools/release/fixtures/dictation_browser_composition_20261010/`:

- Selected baseline `e0cf92a9482dcddcd131cab6076cacdf5b42c43b`: 4 pass, 5 fail.
- New observer with baseline consumer: 8 pass, 1 fail.
- Complete candidate: 9 pass, 0 fail, no skips.

Two actual CEF regression journeys were added: unrelated custom composition end, and removal with stale final/cancel followed by a fresh selected replacement and native undo/redo. Both passed in actual Chromium. Six related lifecycle/refusal cases also passed (eight selected guarded-field cases in 6.447 seconds), and four actual Blink composition cases passed in 4.943 seconds. No skips. The native C++ build passed in 101.681 seconds after a scoped remote blocker build. JavaScript syntax and the Python unit wrapper passed. No local Cargo build, new runtime/model acquisition, microphone permission, deployment or merge occurred.

The lost-artifact blocker was resolved by locating the original owner’s preserved corrected SDK. Before reuse, its archive size (167327081 bytes) and full SHA-256 (`cece35681a153ef16446214a8be35a766a2f9f64a5b9149c63c900857a70c57f`) matched, and all 960 extracted inventory entries were verified. The failed earlier V1 SDK was excluded. The corrected renderer patch digest is `40a39125d44e97d1e0373573fbe77a28ec1254d58f6bb7e2c14eb200614726fc`. The owner’s final qualification receipt records private native/reader success with delivery pending.

This candidate used its own blocker built by `$CARGO --no-fallback --no-fetch build --locked -j2 -p tilo-chromium-blocker --profile thin-app --message-format json` (9m31s; `$CARGO` denotes the authorized remote wrapper with local compilation and fallback disabled), the existing approved qualification image, and the owner’s SDK mounted read-only. Native build output and runtime were private task artifacts. Tests ran through documented bubblewrap private PID/network namespaces within a temporary two-CPU/four-GiB/no-swap service. The build container was verified absent and the service inactive afterward. No sandbox-disabling Chromium flags or setuid helper were added. Production SDK pins and publication authority remain unchanged.

Native logs, receipts and path-normalized wrapper sources are beside the causal logs; the JSON review records raw/canonical hashes and the tested source head. Wrapper copies document execution and require task-path substitution before reuse; they are not new product entrypoints. Source-level causal coverage and actual Chromium coverage are distinct. These tests do not qualify microphone capture, arbitrary third-party editor callbacks, installed signed delivery, or the pending Phonon comparison. Keep the PR draft with `hold` pending integration review and CI.


## CI realm mismatch and retained negative control

Fast-quality run 93073 on source head `ca14a21e2e19d7e818026a618b1236f51e870fcc` failed the original missing-document-observer assertion. The original fixture passes on local Node 26.11.0 but the same source reproduces that failure on existing Node 22.23.1. A minimal VM probe shows that host-side deletion returns true and the host sees undefined on both runtimes; on Node 22 both JavaScript realms still see the observer object, whereas Node 26 sees undefined. The fixture stimulus therefore had not actually removed the subject in CI.

The fixture now creates one explicit persistent VM realm and removes/verifies the observer inside it, matching the actual Blink probe’s in-world deletion. The original `assert.throws` and all production guard code remain unchanged. All nine cases and the Python CI wrapper pass in the existing Node 22 runtime; all nine also pass on Node 26. A disposable missing-observer-refusal mutation fails exactly the original assertion (8 pass, 1 fail), demonstrating that absent-observer admission still breaks the suite. The original Node 22 failure and negative control are retained with raw/canonical digests. The three production-source causal profiles were rechecked with the corrected fixture and retain the 4/9, 8/9, 9/9 results.

This repair changes only the hermetic fixture and evidence. The compiled production C++/JavaScript hashes still match the twelve passing actual Chromium journeys; those journeys were not rerun for this fixture-only change. No runtime or image was acquired: Node 22 was already in the approved CEF qualification image. Final-head normal CI must pass before integration; draft and hold remain required.


## Exact-head lint follow-up and separate idle ownership

Quality run 93152 passed on `89a1ccc2b9025dd58d2088b2a32b7cf6306059b0`, confirming the realm repair in normal CI. Affected run 93151 separately rejected three redundant caller clones in the inherited `live_app_actions.rs` test fixtures at lines 3707, 3813 and 4767. Each broker’s final use now moves the existing value into the provider. The provider retains its required ownership; necessary production/helper and later-use caller clones remain. No assertion, authentication path or production behavior changed.

Remote validation passed: targeted Clippy including shell test targets, strict `--all-targets --keep-going -- -D warnings` Clippy, exactly the three affected semantic cases, and required `check --locked -j2 -p shell --tests` (2m26s). The selected cases cover self-contained nonrepeating remote pages, programmatic resolution without palette entry, and authenticated resource-room scope. The repository’s Nextest runner executed one case per invocation, with all three passing; other library cases were filtered out. The initial validation-only test flag refusal is retained as an invocation error, not a failed or passing test. No completed case was rerun to repair the controller’s reporter-format expectation.

Original CI clone diagnostics, before/after source hashes, commands and portable raw/canonical log digests are retained in the JSON review. Production browser source and the original native evidence remain unchanged.

The same affected run’s separate idle-pair failure reported 15.42 wakeups/s against a 15 limit with zero app messages/repaints. Its existing owner is coordinating through integration. This change neither raises that limit nor manually retries it for luck, and claims no causal idle repair. New normal CI must qualify the candidate; draft and hold remain required.

## Callback replacement and reparenting

Normal required CI passed on `ceeff8104bef5998c382a5fd5b9074ee1360667b` (affected 93383, quality 93384). Two additional regression journeys now exercise real page `beforeinput` callbacks. Replacing the original field with an identically named clone, or removing and restoring the same field before the callback returns, must refuse the final text, leave both original and current values unchanged, and dispatch no input event. Each journey then checks stale final/cancel isolation, fresh cancellation, repeated selected Unicode replacement, and one native undo/redo.

Both final repository fixture cases passed in actual Chromium (2 tests, 1.888 seconds, no skips). The already-qualified private runtime and production source hashes were reused unchanged; only the test fixture and review evidence changed. The new output was separate from the original twelve-case qualification. Private PID/network namespaces, two-CPU/four-GiB limits and cleanup verification remained in effect; the transient service was inactive afterward. No new runtime, model, microphone permission, installed activation or production policy change occurred. Native logs, final fixture hash and raw/canonical receipts are recorded in the JSON review. These two callbacks extend coverage; arbitrary third-party callback behavior remains unqualified. New exact-head CI must qualify this test-only increment; the existing draft/hold remains.
