# Goal-based usability evidence — September 16, 2026

## Result

The expanded tests do **not** accept the desktop as meeting all declared usability gates.

| Evidence layer | Result |
| --- | --- |
| Existing scripted route/geometry/contrast/motion matrix | 48/48 pass, no coverage gaps |
| New deterministic usability properties | 14/18 property/configuration pairs pass; all 18 measured |
| Live screenshot-only agent task completion | 12/12 exact outcomes verified |
| Live agent efficiency gates | 7/12 pass |
| Evaluator regression/negative/boundary tests | 26 pass |
| Host protocol tests | 2 pass; native persistent rename integration passes |
| API-free replay of recorded open-file attempt | Exact document outcome verified |

Combined report: `target/ux-bench/usability-evidence-20260916/index.html`. The report links 162 screenshot/inspection pairs across two deterministic sessions and twelve fresh API-driven sessions. Fresh matrix: `target/ux-bench/evidence-matrix-20260916/index.html`.

## Experiment

OpenAI `gpt-6-astra` received a plain goal, the current screenshot and its own prior actions. It received no application state, semantic selectors, source code or solution route. A persistent native shell widget host applied each pointer/key/type/wheel command to an isolated synthetic profile. Private checks verified the exact requested document body plus Editor identity, or the persisted workspace name `Research`.

Three goals × baseline and 800×700/200%-text configurations × two fresh attempts = twelve trials. Each was bounded to 24 model decisions, 60,000 tokens and 600 seconds; the campaign had a 500,000-token ceiling. Actual final campaign use: **218,921 input+output tokens**. Model calls, prompt hash, timing, decisions, native input counts, captures and replay commands are retained. `build.json` identifies the frozen executable used throughout; a source snapshot is retained under `target/ux-evidence`.

| Goal | Baseline decisions, trials 1/2 | Compact + 200% decisions, trials 1/2 | Efficient completion |
| --- | --- | --- | --- |
| Open Document-000 from desktop | 4 / 4 | 4 / 4 | 4/4 |
| Rename workspace | 19 / 12 | 15 / 13 | 0/4 |
| Find and open Document-060 in Files | 3 / 3 | 24 / 7 | 3/4 |

The file-finding goal permits alternative routes; the last seven-decision trial used keyboard/search rather than wheel input. Counts are decisions, not human task time: full key/character/click/wheel counts remain in the report. Efficiency thresholds were declared before trials: 12, 10 and 18 input decisions respectively, plus limits on no-op/repeated captures. They are engineering guardrails, not measured human norms.

## Findings that the old green matrix missed

1. **Compact large-text legibility:** Files names are severely truncated and glyphs overlap adjacent rows. Editor headings/content also overlap. Inspected evidence: `usability-live-20260916-v2/agent-1-compact-large-text-scroll-file/host/0021.png` and `0025.png`. The agent explicitly observed overlapping text, and one attempt used all 24 decisions. Correct content at the end did not earn an efficiency pass. The screenshot finding is visual review, not a claim of an automated whole-screen typography measurement.
2. **Rename friction:** All four attempts exceeded the ten-decision gate. Recorded actions show repeated selection/deletion/correction attempts. This is a reproducible friction hypothesis, not proof of how humans would behave.
3. **Feedback accessibility gap:** “No results” and a recovery hint are visibly painted, but absent from the accessible snapshot. The corresponding deterministic check fails in both configurations. This is not absent visual feedback.
4. **Palette semantic visibility mismatch:** After Files opens, the palette is no longer painted but can remain exposed as open in accessibility. Repeated observations do not clear it. Source diagnosis: the selected-result path sets `palette_visible=false` without closing palette focus, while accessibility derives visibility from focus. The opt-in diagnostic reproduces this; ordinary protocol tests stay green. Declared geometry overlaps remain unresolved semantic evidence, not proved painted overlap.

Keyboard entry, visible change, Escape cancellation, state preservation, correction of an unsuccessful search, keyboard Files launch and exact document opening pass in both deterministic configurations. Partially clipped controls are explicitly counted/excluded from full-target geometry; an audit with no measured controls cannot pass.

## Validation and integrity

The evaluator rejects false completion, wrong text/provider, missing persistence, hidden/offscreen/modal-obscured outcomes, malformed inputs, missing/blank/corrupt screenshots, excess routes and omitted measurement denominators. Tests also check that the API request contains only allowed observations and that attempts start with fresh sessions/history. The review found and corrected false-positive risks before the final campaign.

An earlier live pilot remains at `target/ux-bench/usability-live-20260916/`: two successful tasks, a rename execution error caused by uppercase `BACKSPACE`, and an interrupted compact attempt. Its campaign note records unknown in-flight usage. Key-name normalization fixed the harness error; no product outcome or threshold was weakened. The complete twelve-trial campaign then ran in a new directory. Journals now retain completed calls and partial input evidence immediately.

No API credential is saved in the source or report. The runner supports a non-echoing key prompt. No changes from this goal have been committed or pushed.

## Reproduce and limits

See [runner instructions](../../ux-testing-bench.md) and [measurement method](../../ux-evidence-method.md). On this machine, building against installed FFmpeg 8 requires:

```sh
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig cargo build --locked -p shell --bin ux-bench --no-default-features
python3 scripts/ux-bench/usability_test.py
python3 scripts/ux-bench/usability.py --mode all --out target/ux-bench/new-usability-trials --key-prompt
```

The native host build and tests passed. A later Clippy attempt was blocked by unrelated concurrent `runtime_host.rs`/browser edits; no clean full-worktree Clippy claim is made. Existing unwind-section linker warnings remain.

These are real application widget/render/input sessions, not mock screens, but they do not traverse the macOS hardware event queue. Human comprehension, physical gestures, all-control keyboard/screen-reader operation and whole-screen contrast/typography remain unproven. Twelve calls to one model are not a human population study. The remaining app defects are reported findings; this task builds and validates the tests rather than silently changing the app until they pass.
