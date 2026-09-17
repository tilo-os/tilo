# Shared Toolbar controller visual gate

Controller individually opened both actual rendered fixtures on 2026-09-16:

- `.superpowers/sdd/2026-09-15-measured-ui-components/task-4-captures/toolbar-ordinary-100.png`: 720×340, dark1×, 530px toolbar. Distinct label widths, normal compact text, separated trailing overflow chip; two-row menu contained, final command readable.
- `.superpowers/sdd/2026-09-15-measured-ui-components/task-4-captures/toolbar-narrow-200.png`: 320×260, dark2×, 290px toolbar. Long command menu occupies available viewport with scrolling; selected last hidden command fully readable, no right/bottom label clipping. Popup legitimately overlays underlying toolbar at constrained height.

Visual fixture gate passes. Source correctness, keyboard/pointer exactly-once dispatch and accessibility require the worker report and independent task review. These are framework fixture screenshots, not claims about apps that have not yet migrated. Archive images/provenance outside the SDD workspace before eventual cleanup.
