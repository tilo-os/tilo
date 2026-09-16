# Task 11 — API Tester navigation and request composition

Status: **DONE_WITH_CONCERNS**. Composition, input, accessibility, normal-scale and true 2× verification are complete. The controller explicitly retained the pre-existing saved-request/draft synchronization defect as an open follow-up (details below). No push or merge.

## Implementation

- Kept the environment selector, New collection and saved objects high in the rail. Save request follows the saved region. Collection / Request / Environment actions reveal the existing management Buttons; only one group opens at a time. All ten original `api.*` management IDs and their `Cmd` / canonical document action sinks remain intact. Accessible names include the selected collection, request or environment.
- One ordered `SidebarItem` inventory carries explicit widget identity and rectangle. Layout, positioning, drawing, retained child enumeration, event routing, accessibility and focus use it. Closed actions have zero rectangles and disappear from children. `ChildSig` includes disclosure, form, paging and visible inventory state. Saved items have pointer wheel scrolling plus keyboard-reachable Earlier / More paging controls. At 800×480 an expanded five-action group temporarily uses the saved-list space; closing the disclosure restores the list. All management targets remain inside the rail.
- Added a local `request_height_override`, drag state and one divider rectangle. Automatic sizing follows nonblank Params/Headers rows, Auth field count, or a useful Body viewport. Measured text metrics drive row/control height. A temporary short-window clamp never overwrites the manual preference. The divider supports pointer drag and focused arrow keys / AT increment/decrement; Request actions contains named Increase / Decrease / Reset request area buttons, all using the same local command path.
- Kept Params/Auth as painted read-only chrome. Body editing, method selection, response modes, variable interpolation and canonical Send remain in their existing paths. Resolved theme text uses the accepted `renderer_input_style` API; response and navigation line geometry expands for true 2×.
- Real button tests exposed two existing app-local routing problems that prevented usable disclosed controls: the root's custom `start_resource_operation` declined child Button/ToggleGroup operations, and TextInput cursor-exit handling stopped hover delivery to later buttons. The app now forwards child operation admission/polling, broadcasts uncaptured cursor moves, and gates drawer pointer input ahead of underlying content. Document mutation logic is unchanged.
- Controller-authorized shared prerequisite, separately committed as **6518c7024** (`fix(ui): apply resolved toggle label scale once`): retained ToggleGroup labels now measure and paint with one normalized resolved style. No shared padding, dimensions or action changes. Its exact report is `task11-api-tester/shared-toggle-report.md`.

Production files: `apps/api_tester/src/app.rs`, `apps/api_tester/src/navrow.rs`; app regression coverage in `apps/api_tester/tests/a11y.rs` and the existing library test module. `responsive.rs` did not require changes; the existing SidebarDrawer setters consume measured rail width. Capture-entry `main.rs` and shared shots files were untouched.

## Verification and TDD evidence

All Cargo commands reused the repository target cache with `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`. No broad workspace suites ran.

| Command after the environment prefix above | Result |
| --- | --- |
| `cargo test -p api_tester --test a11y composition_` (initial RED) | 0 passed, 2 failed: maintenance still visible by default; request divider absent. `logs/app-red.log`. |
| Focused composition iterations | Exposed Reset not reaching its child native operation and pointer Confirm not receiving hover after editing. Local admission and cursor-routing fixes described above made those real-host regressions pass. |
| `cargo test -p api_tester --lib composition_` | 2 passed: content-dependent automatic height / short-window restoration and local-only view commands / hidden rectangles. `logs/local-state-focused.log`. |
| `cargo test -p ui --lib retained_labels_apply_resolved_text_scale_exactly_once` (shared RED) | Failed as intended: resolved 24 px at 2× painted at 48 px. `logs/toggle-red.log`. |
| `cargo test -p ui --lib widget::toggle_group::tests` | 13 passed, 0 failed, including actual glyph sizes at 1× / 2× and existing pointer, keyboard, a11y, action and paint tests. `logs/toggle-green.log`. |
| `cargo test -p api_tester --lib --test a11y` (final affected existing suites, once) | **67 library tests passed**, one capture harness ignored; **13 a11y/interaction tests passed**. No failures or Rust warnings. `logs/final-tests.log`. |
| `cargo build -p api_tester` | Exit 0. `logs/final-build.log`. |
| `python3 .superpowers/app-ui-2026-09-15/task11-api-tester/capture-final.py target/debug/deps/api_tester-9decf21fbb99948b` | Exact ignored unit capture test passed; 15 PNGs plus 15 geometry JSONs. `logs/captures.log`. |
| `rustfmt --edition 2024 --check apps/api_tester/src/app.rs apps/api_tester/src/navrow.rs apps/api_tester/tests/a11y.rs ui/src/widget/toggle_group.rs` and scoped `git diff --check` | Exit 0. |

The shared UI test link emitted the existing macOS `__eh_frame section too large` warning. Build-cache lock delays came from other workspace activity; no fresh target directory was created.

Behavioral coverage includes:

- Closed/open disclosure membership, pointer and Enter activation, expanded semantics, Escape/focus return, hidden action rectangles, open resize, narrow rail open/Escape, and disjoint in-pane sidebar target rectangles at normal and true 2×.
- Drag + arrow-key resize, named Reset, Body/Auth tab switching, Body typing without changing a manual split, response Preview mode, 800×480 clamp and return to the stored preference.
- Paged saved-row keyboard mapping: the focused Auth collection still toggles its own request rows after paging.
- Real pointer Rename collection and Add variable forms reach the canonical document mutations.
- Pointer and focused-Enter Send both reach a local TCP HTTP server, with the expected GET query parameters and interpolated Authorization header. No external network target was used by this regression.
- View commands leave document heads unchanged. Existing canonical-action and outgoing-variable tests continue to pass unchanged.

The added broad behavioral assertions supplement the initial focused RED tests; they are not claimed to have each been independently run against the original production implementation.

## Final capture and binary provenance

Evidence root: `.superpowers/app-ui-2026-09-15/task11-api-tester/`.

- Normal binary: `target/debug/api_tester`
- Normal SHA-256: **88a2211c486af59950de16bda45559af9700a0c4993efbce12fd003f9eb174bd**
- Capture unit binary: `target/debug/deps/api_tester-9decf21fbb99948b`
- Capture unit SHA-256: **86e102835ee03f6db21879be5554cf160392945815060113e1cbe6ccdba0040f**
- Capture HEAD: `6518c702415cfa68de903ddada3ee1b81be047cc` plus this app's verified uncommitted source. `capture-manifest.json` records SHA-256 for each owned source, the shared ToggleGroup source, each PNG and geometry JSON, both binaries, the exact harness command and all runtime homes. The shared checkout contained unrelated changes, which were not included in this worker's commits.
- The script creates isolated task-specific HOME and XDG_CONFIG_HOME / XDG_DATA_HOME / XDG_CACHE_HOME / XDG_STATE_HOME and removes them after capture. Build caches are unchanged.
- **Params with response**, **populated Body**, and **expanded Environment management** were captured at 1200×800, 800×700, 480×900, 800×480 and true text-scale 2 at 800×900. All 15 final PNGs were inspected.
- Management starts with the rail snapped fully open before Driver creation; then a real pointer activation opens Environment actions. There is no in-flight drawer animation in final captures. Body is staged honestly before the first document sync, and the harness asserts the editor contains `Grace Hopper`.
- Trial mid-animation / empty-Body captures were replaced by this final set. Only the final set and hashes are delivered.

Visual findings: three Params rows plus the add-row end immediately above the divider (normal response begins at y268, instead of the large baseline void). Body has a useful editor viewport with populated JSON and visible response. Saved objects precede maintenance in the default wide rail. Narrow expanded management starts at x0 and all five Environment actions fit, including 800×480 and 2×. At true 2× the tabs now paint at resolved 24 px and the row heights / separate response-switch row remain readable. The root owns the separate six standard default variants and independent review.

## Explicit open follow-up: saved-request draft synchronization

**Reproduction:** start `ApiTesterApp::new_demo()` in a real Driver; click `api.nav.3` (POST Create user); click Body. Selection highlight and Body mode change, but the already-bound editor is empty and the method returns to GET on the next sync. A different saved URL is similarly overwritten by the previous active draft. This was found while producing the named capture route and is pre-existing: `load_request` itself was not modified.

**Cause and seams:**

- `app.rs::load_request` (~913) updates local `request`, URL / Body TextInput buffers and selection. `Cmd::LoadRequest` (~2763) calls it without reconciling the bound active draft.
- `Widget::sync` (~3021; bindings ~3046) binds URL and Body to `ApiDoc::url_text` / `body_text` and adopts the existing document text; `reconcile_pickers` (~2950) then reads the old `sel_method` into the just-loaded request.
- `Cmd::ReplayHistory` and `invoke_resource_operation`'s history/request routes have related local setters and should be included in the bounded follow-up's route review.
- Existing setters are `ApiDoc::set_request` (`doc.rs` ~582, including active URL/body data), `ApiDoc::set_text` (~209), and `seed_method_env` for selector strings. Existing request save/rename/delete and environment setters are canonical `ApiTesterAction` operations in `actions.rs`; **there is no canonical LoadRequest draft action today**. A follow-up must choose the existing model's intended draft/document ownership and preserve undo/action semantics, rather than adding a silent alternate write path here.

Controller ruling: leave that repair outside Task 11, stage the Body fixture before sync, and retain this as an explicit open UX issue. Consequently this report verifies selection/nav geometry and editing of a displayed Body buffer, **not successful saved-request content adoption into an already-bound draft**.

## Self-review and limits

Reviewed the owned diff, action ID bindings, explicit inventory mapping, hidden rect clearing, drawer input ordering, measured sizes, local split state, and final capture hashes. The existing immediate-mode app remains large; the task did not introduce a second layout framework. No new Params/Auth editor capabilities, no global spacing changes, no reciprocal-font compensation, and no capture-entry edits. Small windows can temporarily devote the entire saved-list area to the open five-action disclosure; its visible trigger / Escape closes it and restores saved navigation. Independent scoped acceptance remains the controller's responsibility.

Controller visual review noted a remaining 2× checkbox alignment detail: Params check circles still use their original fixed 16 px box at y+4, while enlarged labels use measured row height. Their centers sit about 12 px apart. The controller requested that this be carried into the independent review rather than starting another change before the current commit. It remains an explicit review concern.

## Fix round 1 — wrapped content, alignment and navigation lifetime

Fix base: **04b08caa2**. Status: **DONE_WITH_CONCERNS**, pending controller re-review; the separately scoped saved-draft adoption gap and existing shared linker warning remain open. The reviewed P2 content-fit and P3 alignment findings are addressed. No shared production files, capture entry points, document actions or saved-draft loading logic were changed in this round.

### Implementation

- Params, Headers and Form now shape actual key/value text at their actual column widths into retained `RichTextLayout` cells. Row height, cell rectangles, checkbox/placeholder centers, automatic request height and paint consume that same geometry. `draw_rich` paints the measured glyphs without a second scale/wrapping pass. Form's chip/header offset now matches its painted origin. Cells and rows clip font overhang; no row paints into its neighbor. Variable spans retain accent emphasis using the rich-text bold span style. Manual request-area height still overrides automatic sizing.
- Saved request, history and collection labels now have a measured single-line elision path. The exact elided text is measured and painted unbounded inside a cell clip, so it cannot wrap into the next row. Method/status labels reserve their measured widths. Full existing accessibility names and activation commands are unchanged.
- Checked, unchecked and placeholder indicators are centered from the same row geometry as the key and value blocks, including true 2× and multi-line rows. No font shrink or global spacing change.
- Final affected testing exposed a concrete pre-existing NavRow lifetime defect. `rebuild_nav` dropped the preceding Vec during focused row activation, while both host paths still read old raw widget pointers for focus/undo bookkeeping before rebuilding their maps. The new shaping allocations exposed this as a deterministic SIGSEGV. Controller-authorized app-local repair retains replaced Vec generations until the beginning of `build_layout`, after both `App::rebuild_structure/init_layout` and `FrameHarness::try_relayout` clear their bindings/node maps. Every replacement explicitly flags topology dirty, including same-count changes. Retired rows are never added to the current visible inventory and are released at the next rebuild.

### RED/GREEN and affected tests

All Cargo commands use `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`, with the existing target cache. Evidence is **`.superpowers/app-ui-2026-09-15/task11-fix1/`**; committed Task 11 candidate evidence was not overwritten.

| Exact command after that environment prefix | Result and log |
| --- | --- |
| `cargo test -p api_tester --lib composition_long_` before production changes | **2 failed as expected**. Original 30 px rows painted glyph baselines at 34.03515625 (KV) and 34.53515625 (navigation), beyond their owning rows. `logs/red.log`. An initial test-only compilation attempt used UI's private fixture renderer; that setup was corrected to public `TextRenderer::new` before the recorded behavioral RED. |
| `cargo test -p api_tester --lib composition_long_` after layout changes | **4 passed**: actual glyph containment, Params/Headers/Form measured geometry/automatic height/manual preference/indicator centers, full navigation accessibility names, and actual displayed-row activation. `logs/green.log`. The original glyph assertion was retained when its setup moved from the removed draw helper to the retained production row geometry. |
| `cargo test -p api_tester --lib composition_long_tables_` | **1 passed** with a long hyphenated header key and variable-containing value, at narrow 1× and true 2×. `logs/unbroken-key.log` (the fixture key is hyphenated, not a promise about every possible unbreakable Unicode string). |
| `cargo test -p api_tester --lib --test a11y` before lifetime repair | Library **71 passed / 2 ignored**; a11y process **SIGSEGV** after four successes. `logs/pre-lifetime-tests.log`. This run is explicitly not counted as a successful affected-suite gate. |
| `target/debug/deps/a11y-f4be4afa6af671f1 --test-threads=1 --nocapture` | Serial run also **SIGSEGV**, at focused collection activation in `composition_narrow_escape_paging_and_open_resize_are_reachable`. `logs/a11y-serial-diagnostic.log`. Parallel crash occurred in `composition_pointer_drag_body_auth_and_preference_survive_resize`. Sanitized symbol stacks from both macOS reports are retained in `logs/a11y-crash-stacks.txt`: `NavRow::accessibility` → `CompositionFocus::capture` → `FrameHarness::try_relayout`. |
| `cargo test -p api_tester --lib composition_same_count_` after lifetime repair | **1 passed**: same-count replacement marks dirty, previous focused allocation/name remains live, multiple generations are retained and released when rebuilding after map clearance. `logs/lifetime-focused.log`. |
| `cargo test -p api_tester --test a11y composition_pointer_drag_body_auth_and_preference_survive_resize -- --exact` | Formerly crashing pointer path **1 passed**. `logs/lifetime-pointer.log`. This intermediate build reported a newly unused import, removed before final validation. |
| `target/debug/deps/a11y-f4be4afa6af671f1 composition_narrow_escape_paging_and_open_resize_are_reachable --exact` | Formerly crashing paging/focused-Enter path **1 passed**. `logs/lifetime-paging.log`. |
| `cargo test -p api_tester --lib --test a11y` after all amendments | **72 library tests passed / 2 capture tests ignored; all 13 a11y tests passed**, no failures or compiler warnings. `logs/final-tests.log`. The repeated affected run was required by the observed crash and its authorized repair. |
| `cargo build -p api_tester` | Exit **0**; `logs/final-build.log`. |
| `python3 .superpowers/app-ui-2026-09-15/task11-fix1/capture-final.py target/debug/deps/api_tester-6990ebb2d3749926` | Exact ignored capture test **1 passed**; final 12 PNGs and 12 target-geometry JSONs. `logs/captures.log`. |
| `rustfmt --edition 2024 --check apps/api_tester/src/app.rs apps/api_tester/src/navrow.rs`; scoped `git diff --check` | Exit **0**; final verification log. |

The navigation activation regression mounts the full app for saved-row pointer selection. It also mounts the actual generated history NavRow with its original callback to prove pointer and Enter invoke `ReplayHistory(0)` for the displayed item. This isolates command identity from the separately unresolved history/draft reconciliation behavior; it does not claim full history replay adoption works. Existing full-host focused collection paging, Send, forms, disclosure and divider tests pass unchanged.

No shared ToggleGroup rerun or broad workspace suite was performed. Existing `__eh_frame section too large` warning from the original shared dependency validation remains a known limitation; this round did not investigate or repair the linker. Other workspace builds caused cache-lock waits.

### Fresh final evidence and provenance

- Normal binary: `target/debug/api_tester`, SHA-256 **6cd8cb6db12c2cd1cceef4950391724ef040c333a82f3c08fbadfaf53cde206b**.
- Capture binary: `target/debug/deps/api_tester-6990ebb2d3749926`, SHA-256 **3146b53d442292de530342bfe69a3b1e7b60d47714440c46722d382268e96c1a**.
- Capture HEAD: `fe27e5ab00ccb5d8266677c9912bcf4ae1a0e186` plus the final owned app source. Manifest records exact owned source hashes, relevant consumed shared source hashes, Cargo.lock, binaries and every PNG/JSON. Unrelated shared-checkout changes were not included in this commit.
- Routes: default Params with response, default Headers, long Params, long Headers, long Form, long navigation. Each is captured at **480×900 1× and 800×900 true 2×**. All **12 final PNGs were individually inspected**: wrapped keys and highlighted values fit their rows; checkbox/placeholder centers align; long navigation elides within its target; default short rows retain ordinary density and leave response space.
- Long fixtures are staged before initial document sync. The navigation rail is snapped fully open before Driver creation, so final captures have no in-flight drawer animation. The capture script isolates and then removes HOME and all four XDG homes. The original saved-request-to-bound-draft defect remains explicitly unverified/unfixed.
- The final normal hash was handed to the controller for its six standard default captures. This worker does not claim that separate gate is complete.

### Self-review, exact ownership and remaining concerns

Reviewed the owned diff, actual paint/measurement style and width, provisional-versus-settled geometry, Form offset, row/cell clipping, Unicode-safe elision boundaries, retained full names, callback identity, same-count topology invalidation and both host reclamation boundaries. The existing large app module remains large; no separate framework was introduced. Shared source is read-only for this round. The controller must independently assess the scoped lifetime fix as well as the original visual findings.

Owned production/test files are `apps/api_tester/src/app.rs` and `apps/api_tester/src/navrow.rs` (tests are in their existing modules). Report: `.superpowers/sdd/2026-09-15-app-composition/task-11-report.md`. Fresh evidence files are enumerated individually in `task11-fix1/owned-files.txt`; commit uses only those exact paths. The read-only `.superpowers/app-ui-2026-09-15/api-draft-preflight.md` is a separate controller follow-up artifact, not part of this fix commit. Its ownership/history/Send decisions remain proposals.
