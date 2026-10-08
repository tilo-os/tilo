# Chromium composition mutation guard (source review)

Native IME cancellation can dispatch `beforeinput`, let a page replace a controlled field's value, then delete the new selection. Direct CEF cancellation reaches the same renderer operation. This candidate guards the renderer operation itself and preserves newer page/user edits; it does not change the dictation host's false-success guard.

The pinned Chromium revision is `79460ebecaa5625e57a5fb679a735659e73dc687` (152.0.7977.83), CEF is `708dc140cbc3286826a8abef89dc23a44ff9ea72`, and depot_tools is `ba8bd9c36425cd550ac6a6c9220808fec73352b7`. `recipe.json` binds original and candidate source hashes. `reproducible-input-lock.json` records 158 resolved Git revisions, immutable CIPD/bootstrap identities, toolchain archives, the PGO profile and build profiles.

## Patch and behavior

`renderer-guard.patch` is the complete candidate; its SHA-256 is `185bd544fb0dd8fa2616be1be250833521950da8e8658709d828f31292be609c`. `tests-only.patch` and `fix-only.patch` separate the identical failing-before/passing-after experiment. Apply either the combined patch or the two split patches, never both.

The five runtime files capture the focused target, editable root, complete editable text, selection/affinity and composition generation, then validate that state after script callbacks and immediately before the exact legacy `textInput` default action. A stale operation returns before editing or restoring its old caret. Retiring that operation also prevents empty retries from deleting newer text. Reentrant newer compositions remain authoritative. Undo closes only the old composition, and lifecycle completion stays associated with its original target.

The 26 added tests cover value, selection, focus and composition mutation during `compositionstart`, `compositionupdate`, `beforeinput` and legacy `textInput`; ordinary cancellation/commit, prevention, Undo and repeated actions. The 135 existing controller tests and their assertions remain unchanged.

## Actual source qualification

The final identical `InputMethodControllerTest.*` suite executed 161 tests in each full, remotely built `blink_unittests` binary, with retries disabled and one test per batch:

| Result | Before runtime fix | After runtime fix |
| --- | --- | --- |
| Existing tests | 135 pass | 135 pass |
| Added regressions and controls | 21 assertion failures; 5 pass | 26 pass |
| Ordinary cancellation and style-only commit controls | Pass | Pass |
| Crashes | None | None |

The receipts bind the test source, binary and complete test-summary hashes. The qualification runner used UID 65534, a private network namespace and the exclusive supervisor container's private PID namespace. No test assertion or compiler check was disabled.

The pinned CEF patches also change two upstream test interfaces. `cef-test-support-compatibility.patch` adapts the unchanged test mock's throttle signature; `restore-chromium-tracing-tests.patch` restores two test-only files to their exact Chromium HEAD versions so the complete unit-test target links. Both adapters were identical before/after and reversed before production SDK compilation. Their metadata identifies the exact source hashes and scope.

Separately, all three modified production translation units compiled under the actual official PGO2/ThinLTO profile. This is production compiler/resource qualification, not proof of a completed SDK or signed Browser.

## Replay and delivery gates

Use the pinned public CEF checkout/patch machinery, validate the locked inputs and the post-CEF original source hashes, then apply the resource patches and the two qualification-only adapters. Apply `tests-only.patch`, generate `Release_GN_x64` with `qualification-args.gn`, build the full `third_party/blink/renderer/controller:blink_unittests` target, and run the controller suite. Expect exactly the baseline receipt's 21 added assertion failures and passing original/control tests. Apply `fix-only.patch`, rebuild the same target and run the same suite; all 161 must pass.

For production, reverse only the two qualification adapters, retain the renderer/resource patches, regenerate with `artifact-args.gn`, and verify actual `-fprofile-use` and `-flto=thin` compiler commands. The required SDK target closure is:

```
cef:libcef chrome_sandbox libEGL.so libGLESv2.so libvk_swiftshader.so
libvulkan.so.1 vk_swiftshader_icd.json v8_context_snapshot.bin
chrome_100_percent.pak chrome_200_percent.pak resources.pak icudtl.dat
chrome:packed_resources
```

Use measured concurrency and the existing remote resource supervisor; the generic `libEGL`/`libGLESv2` aliases select test libraries on this revision. Package through pinned `cef/tools/make_distrib.py --ninja-build --x64-build --minimal --no-symbols --no-docs --no-archive`, preserving public headers, resources, locales, wrapper sources and licenses. No partial-package option is permitted. Freeze date metadata/archive ordering and compare independent A/B SDK archives and file inventories after retiring only this task's output between builds. SHA-256 remains mandatory and SHA-1 remains the existing secondary metadata check.

This PR carries source and evidence only. `recipe.json` remains `NOT_QUALIFIED_DO_NOT_CONSUME` with `artifact: null`. The following stages are still pending:

- Complete production SDK A; it has resumed from the preserved approximately 12,168/53,476-action checkpoint (4.955 GiB of owned output) with four compiler jobs after a fresh read-only capacity audit.
- Independent clean production B and byte-identical archive/inventory comparison.
- Real CEF mutation regression and the owner's unchanged native composition/guard/Undo tests on the candidate SDK.
- Actual signed Browser consumer regression through the normal trusted runtime pipeline.
- The repository-required remote `cargo check -p shell --tests` and the normal CI/delivery gates. The authorized source-review PR proceeds independently of the paused heavy build; this check is still pending, not waived or reported as passing.

No consumer lock, binary, production signature, trust policy or live Browser changes are included. PR2974 and the owner's dictation guard/tests are outside this change. Publishing or consuming the SDK remains gated on the pending evidence and normal dependency/signing pipeline. No upstream submission is authorized.

See `resource-budget.md` for the measured peaks and the assumptions behind the 164 GiB admission threshold. Shared maintenance was rejected by automatic approval review and remains unperformed; this source-review delivery does not require it.
