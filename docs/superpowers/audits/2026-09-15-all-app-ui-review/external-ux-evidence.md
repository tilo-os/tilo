# Concurrent UX-bench evidence consumed — 2026-09-16

Read-only integration evidence from the separately user-owned UX-bench task; these are not newly run controller tests and do not close the all-app visual register.

- Source report: docs/superpowers/audits/2026-09-16-ux-findings-repairs.md.
- Frozen executable and current target/debug/ux-bench SHA-256 both `64f6019380980d99e91fa018047934f97c6db4c3356ecefa1c208be02982a37b` at inspection.
- Manifest: target/ux-bench/ux-repairs-results/verification/v2-build.json.
- Controller verified current FileView, PlacesSidebar and NavShell exact hashes equal that manifest. They remain dirty external-owned work; do not stage or overwrite them.
- Parsed final matrix: passed=true, 48 runs, no coverage gaps. Parsed properties: 9 properties ×4 configurations, all36 measured/passed. Parsed rename summary has8 probes. Report includes6 exact/efficient screenshot-only trials (open/rename/find distant file ×baseline/compact200%). No external API campaign initiated here.
- Controller individually viewed deterministic-compact-large-text/host/0009.png and deterministic-large-text/host/0009.png under ux-repairs-v2-deterministic. Visible Files rows and expanded sidebar labels are disjoint. Narrow toolbar still has separate hamburger band; this does not prove all header/grid/rename/manual-column/drag states.

## Downstream ruling

App-text Task9 starts with verification of current external repairs, preserving ownership. Reuse their evidence where source/behavior matches; add only residual app-specific repairs after a fresh source/status check. Task9's populated grid, long names, explicit columns, rename, keyboard and post-scroll drag acceptance still applies. Mail composition Task8's opt-in header ownership and Weather Task13 consume settled NavShell code; do not overwrite this theme propagation fix. Cost if wrong: residual states need targeted repairs, not reversion of accepted concurrent fixes.

Final all-app capture and final current-source bench validation remain pending. Existing bench journeys are not48 distinct apps or human-study evidence.
