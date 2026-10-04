# UX Findings Repairs Implementation Plan

**Goal:** Fix all product defects identified by the screenshot-agent campaign and deterministic usability checks.
**Spec:** docs/superpowers/audits/2026-09-16-ux-evidence-results.md
**Architecture:** Repair product layout and interaction sources, retain the existing independent outcome and efficiency gates, and verify fresh rendered evidence.
**Global constraints:** Preserve concurrent changes; no weakened checks; no credentials in files. Document any failed validation honestly.

### Task 1: Repair reported product defects
- [x] Reproduce compact 800x700 200% text Files/Editor overlap, rename correction friction, missing no-results accessible feedback, and stale palette semantics using preserved artifacts and current source.
- [x] Add focused failing regression coverage at root causes before fixes. Inspect prior live rename actions to distinguish app defects from model mistakes.
- [x] Fix layout sizing/measurement, responsive file identification, normal platform text-selection/editing behavior, accessible feedback, and consistent palette close state. Keep app semantics and rendered state aligned.
- [x] Fix audit F3/F4/F6: preserve compact workspace/tab/file identifiers, prevent persistent Help badge from occluding app content, distinguish workspace settings from global quick settings. Keep compact empty-desktop heading below topbar.
- [x] Fix F9 discovered by fresh screenshot trial: launcher category chips, result rows/sections and footer overlap at200%; add measured geometry and rendered typography regression, preserving requested enlargement.
- [x] Fix F10 expanded Files sidebar heading/navigation overlap at200%, with painted text scale and row/header geometry regression.
- [x] Repair NavShell actual-theme propagation to child layout, uncovered by native capture: measured200% geometry must survive container placement. Add propagation regression and scoped container tests.
- [x] Run focused tests and rebuild ux-bench using FFmpeg 8. Record exact changed files and evidence in task report.

### Task 2: Validate and independently review
- [x] Run fresh deterministic properties and existing matrix. Inspect compact screenshots for every reported overlap and collect direct rename route input evidence.
- [x] Re-run live agent campaign if authorized credential is available; otherwise identify live rerun limitation and use recorded-input replay plus native goal checks.
- [x] Independently review spec compliance and code quality; fix actionable findings without weakening expectations.
- [x] Publish local before/after evidence and concise results with explicit limits.
