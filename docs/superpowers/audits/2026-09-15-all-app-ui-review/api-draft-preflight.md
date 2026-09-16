# API Tester — active-draft adoption preflight

Date: 2026-09-16. Read-only investigation after Task 11; **proposal, not an implementation or approval**. Production source was not changed. No tests, builds, captures, commits or agents were run for this preflight. Source inspected at shared-checkout HEAD `873d524ae476902462213cc71602d60918bf3589`; symbol names below are durable references and line numbers are approximate.

## Finding and ownership decision

Task 11's report records a real Driver reproduction: start `ApiTesterApp::new_demo()`, click `api.nav.3` (POST Create user), then Body. The selected row and Body mode change, but the already-bound body is empty, method returns to GET on sync, and a different saved URL is similarly overwritten. Task 11 intentionally left this pre-existing defect open. Its final Body captures stage the fixture **before first sync** and are not evidence of successful adoption into a mounted editor.

Existing ownership is internally inconsistent:

- `actions.rs` module documentation declares which saved request is loaded to be per-peer presentation; tabs, response view/scroll, folds and rail are also local. `ApiTesterOperation::ReplayHistory` explicitly promises local View navigation with no collaborative mutation.
- The actual URL and Body editors bind to **one shared active request's root Automerge Text objects** (`url`, `body`). The method selector similarly binds to `sel_method`. `ApiDoc::set_request` also stores active request metadata and keyed params/headers/form. This is not a separate per-peer draft schema.
- `ApiTesterApp::selected` is local navigation context, while `request` is an in-memory projection used for method/body mode/auth/params/header behavior. The active metadata is adopted at initial mount, not refreshed on each subsequent document change.
- Saved request records are independent durable collection entries. Loading should not rewrite a saved record. Network Send remains an external effect and the response remains local.

**Recommended scoped ownership, subject to the controller's decision:** retain the existing shared active-draft schema. Treat copying a saved snapshot into that draft as a canonical document action, and keep the selected row, active tab, response view, folds, disclosure and manual request-area size local. A peer may see new shared draft content without acquiring the initiating peer's selection or tab. Document this distinction explicitly; merely calling load a View does not make writes to the shared draft local.

If the intended product behavior is truly independent per-peer loaded content, the follow-up instead needs a local/per-peer draft binding and undo design. That is a larger ownership change and must not be inferred from this layout fix.

## Existing storage, action and undo seams

| Source | Existing behavior and implication |
| --- | --- |
| `apps/api_tester/src/doc.rs::set_text` (~209), `url_text`, `body_text` | Diff-splices existing Text values, retaining object identity. This is the appropriate storage primitive for a whole-draft action; replacing editor objects would discard the live binding contract. |
| `doc.rs::set_request` (~582) | Writes active id/name/method/auth/body_mode, URL/body Text, keyed params/headers/form. Does not update `sel_method`. Production use is initial `seed_workspace`; other calls found are tests. |
| `doc.rs::request` (~609) | Reads active metadata, keyed values and live URL/body. Only nonempty live text overrides the legacy base; clearing a migrated legacy URL/body needs a regression before assuming empty reads are correct. |
| `ui/src/widget/text_input.rs::set_text` (~1014) | Programmatic local replacement clears `dirty`; it is not a queued bound-document edit. Subsequent sync can overwrite it from the existing Text object. |
| `app.rs::perform` (~980) | Dispatches `ApiTesterAction` through `ActionHost::dispatch_with_context`; then `adopt_workspace`. The latter intentionally excludes the active draft. |
| `actions.rs::ApiTesterAction`, `Action::apply`, registry | Existing canonical mutations cover saved requests, collections, environments, variables and active environment. There is **no active-draft load/replace action**. Apply is deterministic; IDs are authored outside apply. |
| `ui/src/action_host.rs` | Serializes and dispatches through the canonical executor, retains the pre-action heads after successful admission, exposes a one-shot committed-action bracket. |
| `app.rs::take_committed_action` (~3501); `ui/src/undo.rs::record_dispatched_action` (~220) | Host consumes the bracket and records author-aware document undo. Direct calls to setters from a local load handler would bypass the explicit action boundary. |
| `ui/src/headless.rs` Driver host tick and undo | Event/AT control dispatch, operation admission and committed-action recording precede relayout/sync. Undo/redo updates document state and relayouts. A correct document undo alone does not restore stale app metadata unless sync reads it. |
| `ui/src/bound_dispatch.rs::execute_resource_operation` (~950) | Document operations decode/apply canonical actions and report before/after heads. View/Effect operations call `start_resource_operation_with_document`; no handler produces Unknown. Remote actors cannot invoke another participant's private View. |
| `Widget::start_resource_operation_with_document` default | Forwards to `start_resource_operation`. A mutable document entry exists, but using it does not justify an undisclosed document write under a View contract. |

Current method ownership also needs deliberate resolution. `reconcile_pickers` reads `sel_method` into the local request, while active-record `request.method` is separate. A replacement action must update an atomic mirror or use a single authoritative method projection. Refreshing the record on every sync without addressing this would undo ordinary picker edits or reintroduce stale method values. Do not use the combined `seed_method_env` helper to load a draft: it also writes environment selection and can overwrite unrelated state.

## Complete app route inventory

The production `LoadRequest`/`ReplayHistory` routes found in API Tester sources are below. Tests and capture staging are not additional user routes.

| Entry | Actual path and current behavior |
| --- | --- |
| Saved row pointer, Enter/Space, or AT activation | `compute_nav_meta` emits `Cmd::LoadRequest(ci, ri)`; NavRow pushes the common sink; `drain_commands` calls `load_request`. |
| `load_request` (~913) | Clones local saved record, calls both TextInput `set_text`s, replaces local `request`, stores local selection, resets tab/toggle to Params and rebuilds navigation. No document mutation, method-selector write or undo boundary. |
| History row pointer, Enter/Space, or AT activation | `Cmd::ReplayHistory(hi)` in `drain_commands` sets local method and URL/input only. It retains existing body/auth/params and selection/tab. It does not clear the error as the resource variant does. |
| Send button / current-request exported Send | `SendActiveRequest`, `OperationKind::Effect`, calls `send`; `outgoing` snapshots local request with current editor URL/body and environment substitutions. It does not load another request. |
| Saved request exported Send | `SendRequest { collection_id, request_id }`, Effect. `invoke_resource_operation` resolves IDs to local collection entries, calls `load_request`, then immediately `send`. Missing/invalid IDs error. Because Send snapshots before the next editor sync, its first outgoing request may contain the saved values even when the subsequent displayed draft reverts. Do not infer failure timing from sidebar load. |
| History exported replay | `replay_history_operation` publishes history resource action `replay`, label “Open request in builder”, View, immediate execution. The legacy `invoke_resource_operation` branch resolves history ID and sets method/URL locally, clears error and marks dirty. |
| Actual host admission for API operations | App `start_resource_operation` first forwards to visible retained children, then accepts **only Effect + `api_tester.operation`**. View replay falls through to None; default document-aware forwarding has no legacy fallback. Host dispatch therefore cannot reach the published history View branch. This is a source-derived existing admission defect, not newly runtime-tested here. |
| Saved request palette resources | Exports rename/delete/send, **no LoadRequest/open-draft action** today. Any new open action needs the chosen ownership kind and the canonical action payload. |
| Collection palette “Save current request here” | `provide_resources` reads `ApiDoc::request`; `save-active` copies that snapshot with a new authored ID into `SaveRequest`. It can disagree with currently displayed local metadata. It is not an active-draft adoption route. |
| Save request form | Creates a saved snapshot from local `request` plus current URL/body inputs and uses canonical `SaveRequest`; does not fix the active record. |
| Initial seed/adopt | `seed_workspace` writes the initial active draft. `adopt` reads a nonempty document once. Neither is a valid substitute for loading into an already-bound, edited draft. |

`Widget::sync` order matters: initial seed/adopt; bind/sync URL and body; drain AT commands; sync/reconcile pickers. Pointer/keyboard handlers also drain commands immediately. A new draft action dispatched by AT after the editor sync must still reach the visible controls without a stale frame or a further user event. Action undo recorded from commands drained during sync must be tested through the real host boundary, rather than assumed from `perform` alone.

History is not a full request snapshot: `model.rs::HistoryEntry` contains id, method, URL, status, duration and when; it has no body/auth/headers/params. A follow-up must explicitly retain method/URL-only behavior or choose a fresh default draft. It cannot promise to reconstruct the sent body from current data.

## Canonical implementation options (proposed only)

### A. Shared draft copy — smallest fit to the persisted schema

Add one serializable action, provisionally `ReplaceActiveRequest { request: Request }`, carrying the authored complete snapshot. Reuse `ApiDoc::set_request`, synchronize the chosen method authority in the same action, and route sidebar adoption through `perform`/ActionHost. Carry source IDs as optional provenance only if needed; do not mint IDs inside apply. A snapshot payload expresses the content the user chose and avoids re-resolving a later-edited saved record during replay. Decide explicitly whether active request identity remains the saved request ID, matching current load behavior; do not invent a new identity silently.

Add a narrow active-draft projection refresh after document changes, including undo/redo and remote changes. Keep TextInputs bound to existing Text objects. Do not wholesale-call `adopt` on every frame or reseed collections/environments. Refresh only authoritative draft metadata and let live editors sync their objects; preserve local view state. Review method picker reconciliation so it cannot write old state back after undo.

Use the same action encoder for any new palette “Open in builder” document action. Keep existing stable resource IDs and saved-record mutation contracts. Assert that palette Save current request and visible Save see the same active method/body/URL after load and after edits, rather than treating a correct-looking editor as sufficient.

**History requires a separate explicit contract choice:** copying its method/URL into a shared draft is a Document action, so it cannot remain an honest private View operation. Either reclassify/document the copy behavior with the corresponding canonical action and update its tests, or keep a separate local preview/navigation operation and add an explicit shared-copy command. Do not queue a hidden shared mutation behind a View invocation. This preflight does not authorize that change.

**Saved Send should keep a clear Effect contract:** preferably send an immutable saved snapshot without first replacing the shared active draft. If the product intentionally makes Send also adopt the shared draft, specify and test the compound document action + external effect, including undo of the draft but never replay of the network request. Do not automatically reuse a new document-writing `load_request` inside the existing Effect handler without this decision.

### B. Independent per-peer draft

Make the loaded method/URL/body genuinely local or per-peer, give that draft appropriate local undo, and change palette Save/Send to use it. This aligns with the existing presentation comments and history View promise, but changes collaboration/persistence and is larger than a setter repair. The controller should scope it separately if chosen.

### Insufficient shortcut

Adding `ApiDoc::set_request` and `seed_method_env` directly inside `load_request` can make one click appear to work but bypasses canonical action/undo admission, rewrites environment state, leaves metadata undo refresh unresolved, and changes the meaning of saved Send/history operations. Calling `TextInput::set_text` again after sync only hides the ownership conflict. Neither is an adequate fix.

## Minimal acceptance and meaningful RED regressions

These are proposed tests, **not executed results**. First establish the ownership option above. For option A:

1. **Already-bound saved adoption through the real host.** Mount `new_demo` in Driver, complete initial sync, then make an unsaved draft A through real controls: method differs from saved B, URL differs, and Body contains a unique marker. Commit those edits before activation. Do not seed B into the app before Driver creation. Use a saved B with another method, nonempty distinct URL and nonempty JSON body; click its actual row. After host sync and another idle/layout sync, assert visible method label, URL value, Body value, body mode, `ApiDoc::request` and exact root Text values all equal B. Inspect outgoing snapshot (or local HTTP fixture) to prove Send would use B. Baseline should fail on bound text/method persistence, reproducing Task 11 rather than merely checking selection.
2. **One undoable copy boundary.** With the same mounted app, assert canonical action delivery/admission and one copy boundary independent of preceding typed edits. Use host Undo: A's exact method/URL/body and draft metadata return in both document and visible controls after repeated sync. Redo restores B. Do not count a pass that only inspects document heads. Saved collections/records and active environment remain unchanged; root URL/body Text object IDs stay stable. Selection/tab are local and must not be serialized or implicitly restored by document undo; specify a nonmisleading selected-row behavior separately.
3. **Keyboard and AT parity.** Activate B through focused Enter/Space and accessibility Activate. Confirm the same canonical action and result, including an undo boundary for commands first drained during sync. This catches routing/order differences that a direct private `load_request` unit call misses. Pointer, keyboard and AT need not each repeat every long assertion if the common sink and host boundary are evidenced.
4. **Ordinary edits after load.** Change method, URL and body after adopting B, then sync twice. Check visible draft, outgoing and exported `save-active` snapshot agree. Undo/redo the adoption must not trigger picker reconciliation that writes stale metadata back. This prevents a refresh-only fix from breaking method selection or retaining stale active-record metadata.
5. **Clear values and data isolation.** Adopt a snapshot with empty URL/body, including a migrated legacy seed case if supported. Verify actual empty text, not a legacy fallback. Distinct params/headers/auth/form/body mode are adopted without mutating the saved source; environment and local manual split remain stable. No new Params/Auth editing capability is needed.
6. **Canonical replay proof.** Encode/decode the new action through the registered executor against a cloned seeded document; compare authoritative active projection and stable Text identities. Verify deterministic replay and ordinary action undo. This complements, not replaces, the mounted-host regression.
7. **Resource route contracts.** Test published history replay through the real resource front door, not `invoke_resource_operation` directly: baseline should expose current View admission failure. Under the selected contract, assert correct admission, no accidental HTTP request, and the specified method/URL-only/body behavior. Test saved Send against a local server and inspect both immediate outgoing content and the displayed/shared draft after subsequent sync. Include stale/missing saved/history IDs and busy Send; preserve its accepted/completed lifecycle.

A bounded first repair may implement only saved-row adoption plus canonical draft/undo projection and explicitly leave history/Send contract changes for a second task. If so, isolate the common helper to avoid changing those routes accidentally and retain both open issues in its report. Do not declare complete route parity until the actual host resource tests pass.

## Adjacent findings and limits

- The published history View's admission gap is established by source tracing here, not a fresh runtime failure. Existing `history_records_are_addressable_with_an_honest_view_operation` verifies resource publication/kind, not full host delivery.
- `poll_worker` builds history from the current request/outgoing state on completion; switching drafts while a request is in flight may record different metadata than the sent snapshot. This is adjacent to saved Send behavior, outside the minimal adoption repair unless that route changes.
- Local `selected` uses indices. This preflight does not redesign selection identity under concurrent collection edits.
- Initial mount, staged capture fixtures, private helper tests and document-only setter tests cannot validate the reported already-bound failure. Real host activation plus repeated sync and undo is the primary acceptance evidence.

Primary prior evidence: `.superpowers/sdd/2026-09-15-app-composition/task-11-report.md`, section “Explicit open follow-up: saved-request draft synchronization”. All proposed changes await the controller's separately scoped task.
