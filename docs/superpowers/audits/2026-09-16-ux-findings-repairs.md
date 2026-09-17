# UX findings repairs — September 16, 2026

## Scope

Repairs address the demonstrated failures in the earlier usability campaign, then additional overlaps found by fresh screenshot-driven trials and a four-configuration visual audit. The original artifacts remain unchanged.

| Finding | Product repair |
|---|---|
| Files rows/headings/columns overlap at200% | Scale row geometry and detail-column requirements; apply text enlargement once; retain distinguishing filename suffixes. |
| Editor heading/hint/body overlap | Stack measured heading and hint; use enlarged line spacing for content and local/remote selection/carets. |
| Compact identifiers disappear | Give workspace name priority over redundant count; preserve document-number suffixes in tabs; keep complete identities in semantics/settings. |
| Help badge obscures app content | Suppress badge and its hit target while app groups are present; keyboard Help remains available. |
| Rename selection invisible | Paint actual selection and caret; expose selection range to accessibility; invalidate after select-all; collapse selections correctly with Left/Right. |
| Workspace/global settings confusion | Distinct workspace pencil affordance. |
| No-results feedback inaccessible | Expose empty-search title and recovery hint in accessible content. |
| Dismissed palette remains exposed | Close palette focus whenever host visibility closes. |
| Compact home heading overlaps topbar | Use two-column compact layout so content no longer shifts above chrome. |
| Launcher text overlaps at200% (fresh agent finding) | Apply enlargement once; allocate scaled rows, full category labels and footer separately; fit viewport and update after height-only resize. |
| Expanded Files sidebar heading overlaps Root (visual audit finding) | Scale sidebar heading/row/hit geometry and preserve the actual theme through NavShell child placement. |

## Why the second pass was necessary

The first repaired build passed48 scripted matrix runs and36 deterministic property checks. Three fresh compact screenshot-only agents also completed their goals efficiently (rename5 inputs, open4, distant-file4). Yet the open-file agent identified severe launcher text overlap. A subsequent visual audit found the expanded Files sidebar overlap too. These failures were retained, repaired, and require fresh final evidence; successful task completion alone cannot establish legibility.

The fresh agent conduit supplies only the goal and screenshot paths, accepts visible mouse/key/type/wheel actions, and saves private native state for independent outcome checking. Each agent starts with a fresh context and profile and is instructed not to inspect source, semantics, prior results, or solution routes. These are Codex subagent trials, separate from the earlier external OpenAI API campaign; no new API credential is used. Input and efficiency limits remain unchanged.

## Final evidence

Frozen executable SHA-256: `64f6019380980d99e91fa018047934f97c6db4c3356ecefa1c208be02982a37b`. Final matrix48/48, property checks36/36, and native rename probes8/8 pass. Six fresh screenshot-only trials all complete within the unchanged efficiency limits. Independent final review accepted all confirmed repairs, with no blocking findings. Report at `target/ux-bench/ux-repairs-results/index.html`.

| Fresh trial | Baseline inputs | Compact200% inputs |
|---|---:|---:|
| Open Document-000 |4|4|
| Rename workspace |5|5|
| Find/open Document-060 |3|4|

Both distant-document trials used actual wheel inputs. All document bodies/provider identities and persisted names were checked independently. Final supplemental validation:321 scoped tests (236 Files,37 NavShell,46 palette,2 shell regressions) plus4 native host tests; earlier focused tab/editor/settings checks also passed, with overlapping suites not summed into one inflated total.

## Limits

Findings concern the tested native widget screens/configurations. No claim is made about every desktop surface, all possible filenames/viewports, physical input-device delivery, or human comprehension. Intentional ellipsis that preserves identity elsewhere and partial rows at scroll boundaries are distinguished from text painted over neighboring content.

Remaining observations: a newly inserted tab fades in over220ms and is legible once settled; truncated identifiers retain a distinguishing suffix or complete identity elsewhere; clipped list-edge rows indicate scrolling. Suggestions for a closer save hint/button or file-opening hint were not demonstrated task failures. They remain documented as design suggestions, not silently counted as repaired defects.

No commits or pushes were requested for this repair pass. Concurrent unrelated work is preserved.
