### Spec Compliance

- ❌ Issues found: startup preference loading is correctly gated, but it is omitted from the production idle-polling predicate. The required startup polling can stop before loading settles (`apps/imager/src/setup/internal_download_flow.rs:142`, `:583`; `apps/imager/src/setup/view.rs:1934`). See Important 1.
- ✅ The reviewed storage/worker slice otherwise matches the binding contract: bounded opaque bytes, fixed legacy path, no-create/no-follow load, independent namespace/publication mapping, Unix atomic publication, Windows NamespaceUnproven, worker-local hooks, save-before-authentication, retained cancellation facts and matching-token acknowledgement (`crates/packaging/install-artifacts/src/preference.rs:17`, `:46`, `:75`, `:151`; `src/cache/coordinator.rs:291`, `:314`; `apps/imager/src/setup/internal_download_flow.rs:520`).
- ✅ The unchanged SDK already explicitly exports the final preference DTOs and Client, so its absence from this Task 2 diff is justified (`crates/app/tilo-app/src/install_artifacts.rs:2`). The facade signatures and additional API fixture/coordinator call-site edits fit the authorized task (`crates/packaging/install-artifacts/src/lib.rs:52`; `tests/api_surface.rs:5`; `src/cache/coordinator/tests.rs:89`).
- ⚠️ Native Linux/Windows execution, missing production macOS cache, G02 authority and Task 3 boundary/packaging gates cannot be certified by this task diff. The Windows success branch and conditioned assertion are source evidence only (`crates/packaging/install-artifacts/src/preference.rs:108`, `:274`). The parent's current boundary guard remains red (85 diagnostics), assigned to Task 3; this review does not claim it passed.

### Strengths

- `crates/packaging/install-artifacts/src/preference.rs:46`: load retains the parent handle, uses the shared no-follow child open, checks metadata before allocation/read, and caps the read at 4097 bytes. JSON remains app policy.
- `crates/packaging/install-artifacts/src/preference.rs:87`, `:163`, `:186`: real provision/admission/publication calls preserve their separate knowledge states; a failed commit never triggers compensating deletion. Windows success explicitly stays NamespaceUnproven (`:107`).
- `crates/packaging/install-artifacts/src/cache/coordinator.rs:314`, `:338`: saving precedes authentication, and the existing terminal path keeps the completed commit or error/uncertainty even when cancellation wins the phase. The actual-worker cancellation/failure regression asserts these facts and absence of network/cache effects (`src/cache/coordinator/tests.rs:1286`).
- `crates/packaging/install-artifacts/src/test_support.rs:193`: observers and failure injection wrap the actual save on the worker thread; holds have a ten-second bound. Integration tests require observed checkpoints and independently reopen published bytes (`tests/preference_publication.rs:66`, `:108`).
- `apps/imager/src/setup/internal_download_flow.rs:524`: saved state advances only for the matching token's retained commit, once even after status coalescing; tests reject foreign tokens and retain the namespace warning (`:1336`, `:1361`).

### Issues

#### Critical (Must Fix)

- None found in this bounded source slice.

#### Important (Should Fix)

1. **Preference loading does not request continued production polling.** `apps/imager/src/setup/internal_download_flow.rs:583` only reports a pending command or operation phase. A newly constructed flow has `preference_loaded == false`, no token/phase and no pending command (`:140`), so it returns false while the worker is still loading. `SetupApp::needs_idle_sync` delegates to this predicate (`apps/imager/src/setup/view.rs:1947`); preference completion only notifies the worker mailbox condition variable (`crates/packaging/install-artifacts/src/cache/coordinator.rs:296`). If the first UI sync observes Loading and other background work has settled, this operation supplies no reason for another sync, leaving start/resume disabled until unrelated UI activity. Include unsettled preference loading in the existing pending/idle contract, and add a regression through the actual `SetupApp::needs_idle_sync` path before loading settles and after it settles. The new flow test manually calls `wait_preference` and the view fixtures explicitly pre-wait (`internal_download_flow.rs:1344`; `view.rs:2361`), bypassing this scheduling condition.

2. **A failed or uncommitted cancelled save strands the flow with no retry command.** `apps/imager/src/setup/internal_download_flow.rs:379` assigns both `pending_key` and `token` before persistence completes. On a terminal save failure without a commit, `accept_status` retains both and correctly leaves `saved` unchanged (`:524`). However, `can_start` requires `token.is_none()` (`:333`), and `can_resume` requires the old saved key to equal the offered key (`:310`). For a fresh selection, neither command is available; discard also requires a saved key (`:453`). This regresses the old synchronous-save path, where a failed write returned before token admission, and also affects cancellation before a commit. Permit explicit retry after a terminal uncommitted attempt, while keeping the prior failure, exact publication/namespace uncertainty and old saved key visible until that retry. Never promote an uncommitted key to saved. Add a real-facade flow regression that observes save failure, verifies the retained facts/no network or cache effects, and then proves a new explicit start can be admitted and only a matching successful commit advances saved. Cover cancellation without a commit as well.

#### Minor (Nice to Have)

- `crates/packaging/install-artifacts/src/lib.rs:93`, `src/preference.rs:11`: final normal macOS compilation reports unused `saved_key` and newly added unreachable preference plumbing alongside the larger existing warning debt. Evidence: `task-2-final-preference-normal.log:12`, `:427` (100 compiler warnings / 101 warning lines overall); the frozen report also explicitly records substantial service/app warnings. Keep this visible as non-pristine validation; scope any cleanup to introduced host-cfg warnings without suppressing unrelated diagnostics or widening this task into dependency cleanup.

### Assessment

**Task quality:** Needs fixes.

**Reasoning:** The persistence and cancellation implementation uses the right shared primitive and preserves typed effects, with strong production-path fault evidence. Two consumer state/scheduling regressions prevent the app from reliably reaching or retrying that implementation and should be fixed before Task 2 acceptance.

### Checks and evidence boundaries

- Read the complete 1811-line snapshot-relative diff once in consecutive nonoverlapping segments, against the Task 2 brief, design and frozen report. No git operations, suite reruns or product-source mutations were performed.
- Verified the supplied report SHA-256 `56f8200cc5bbf33237c48106c504f0ecec3729b67926e44d98d1eeaf0ac627d0` and diff SHA-256 `87d4477f7c1eab92c1124032a9f8d5dbab821866ed8ba1befe517a239c0eb7ad` directly.
- Focused context checks were limited to named risks: startup scheduling (`internal_download_flow.rs:133`, `:583`; `view.rs:675`, `:1750`, `:1934`; `ui/src/app.rs:12519`), retry admission after failed persistence (flow command gates and token/pending-key assignments), cancellation retaining errors (coordinator terminal block `:338`), shared API availability/no-follow semantics (`crates/system/atomic-file/src/directory.rs:26`, `:59`; `src/lib.rs:65`), and already-complete SDK exports (`crates/app/tilo-app/src/install_artifacts.rs:2`). Changed-file context was read only where the supplied hunks cut off these functions or their required call-site context.
- Accepted parent-verified final evidence without rerunning suites: preference unit 10; integration normal 1/support 6; API normal 2/support 3; Imager flow 20/view 3/check-tests exit 0. Coordinator 29 passed before the final added cancellation regression; that regression is included in the final preference-unit 10. Counts are not native Linux/Windows qualification.
- Read `task-2-shell-checkpoint-retry1/receipt.json`: `cargo check --offline --locked -p shell --tests` exited 0 in 21.964 seconds, 2536 inputs, no changed inputs. Parent independently verified its receipt/raw log/snapshot hashes. This supersedes the frozen report's pending-shell sentence; the earlier drift-invalidated run is not acceptance evidence.
- Warning output is retained as the Minor finding above. No test run was needed to establish the two flow defects: their missing scheduling/admission predicates are visible on the production path, and the existing passing tests bypass or stop before those conditions.
