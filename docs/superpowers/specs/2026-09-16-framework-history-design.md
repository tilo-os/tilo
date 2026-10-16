# Framework-owned History mode

## Approved outcome

The user selected a framework-owned History experience for every application, combining activity and historical versions. Their implementation instruction authorizes this design and migration, using subagent-driven development and coordination with the existing runtime and action tasks.

The shared host supplies the entry point, panel, controller, historical banner, comparison, return-to-live, and reviewed restoration. An application supplies its domain rendering and action semantics, not a separate history workflow. Tilo embedded and standalone applications use the same implementation.

## UI behavior

- A standard History entry point is automatically available for managed persistent resources.
- Opening History shows grouped activity beside the current canvas: author, time, and factual description. Scope is explicitly named by resource. Unknown timestamps/authors stay unknown.
- Selecting an entry shows its resulting historical state in an isolated instance of the normal application renderer. The banner says it is historical and offers Back to live. Editing/effects are unavailable; safe navigation, scrolling and zoom remain possible.
- Live state, selection, unfinished interaction and view state remain separate and continue receiving collaboration. Returning to live restores that view; updates while previewing are indicated without advancing the selected version.
- Comparison switches between the exact before/after snapshots of the selected change or group. Domain-specific visual diffs may extend this but are not required.
- Restore opens an explicit review naming the resource and selected version. Confirmation checks current authority and the exact reviewed live frontier. If the frontier changed, refresh review before applying. Restore is a new authored, undoable change; no old change is removed.
- At narrow widths, activity and preview are separate selectable pages within the same history surface. Controls remain reachable by keyboard and expose accessible labels.
- History is pinned to its selected resource while previewing. A resource change cannot silently retarget a restore. Resource-level state is the initial scope; existing workspace activity can navigate to it.

Use the existing UI HIG: history activity is content/master-detail, not a narrow overlay drawer. Compose standard Button, ActivityTimeline, ModalDialog and navigation primitives so focus, keyboard, accessibility and theming remain shared. Do not hand-paint interactive rectangles. Existing workspace History shortcuts retain their meaning; app history entry points must not steal them.

## Runtime boundary

Build on `tilo_actions::replay`, `ui::history_source`, `tilo_doc::history`, `AppComponent`, `RecordStore`, and canonical bound dispatch. Do not create a parallel action log or migrate stored change graphs. Historical reads use immutable snapshots/forks. Reconstruct state from existing CRDT history without reexecuting external effects.

Current apps expose two hosting paths: Notes defines an `ApplicationDefinition`/`AppComponent`, but its production launcher also still uses the legacy path. The inventory finds 47 GUI applications using `LaunchSpec::new`/`new_with_args`, including six custom live handlers that construct `App` directly. The implementation must cover the common App construction path, headless drivers, nested TileHost roots and bound components. Adding History only to `AppComponent` would not satisfy this specification. No wholesale application-lifecycle migration is required merely to display history.

A shared controller owns resource identity, activity/group selection, pinned heads, before/after bytes, live-update tracking, errors, and a restore-review token. A snapshot contains no live record, permission, invocation, persistence, or publication handle. Keep the controller independent of window/compositor state.

The app host owns the interactive surface and the preview lifetime. The preview renderer is a separate instance created through an explicit read-only rendering contract. Default unsupported previews must not silently satisfy app migration; each managed app must supply a meaningful domain preview or an explicit truthful limitation for inherently external/non-versioned content. External web pages and terminal processes cannot be reconstructed by replaying their effects.

All document-changing operations, including restore, enter current canonical authorization and owned action frames. Preview cannot enqueue controls, invoke external operations, publish collaboration, autosave, or acquire live mutable record handles. Disabling buttons alone is not enforcement. Closing/switching components invalidates pending history actions.

Restore preserves record identity and runtime metadata. It targets application content, not capability grants, receipt records, collaboration routing, or view state. It must prove the resulting content matches the selected version for supported schema types; partial patch support must return an error instead of fabricated success.

## Migration

1. Add shared controller with graph-based selection, exact snapshots, live-update tracking, and restore review validation.
2. Add enforced preview contract and the automatic shared host surface. Integrate canonical authorized restore and lifecycle teardown.
3. Inventory and migrate every managed persistent app. Remove/redirect competing app-specific history controls, starting with text and non-text representatives but continuing through the inventory. Preserve saved history and existing view restoration.
4. Verify native and headless host paths, interaction/accessibility, multiple resource scopes, concurrent edits/revocation, and restore persistence/undo. Document any inherently non-versioned external content precisely.

## Required evidence

- Real history graphs, including concurrent changes, stable exact selection, grouping and unknown entries.
- Preview selection/comparison leave live heads and bytes unchanged; live remote edits do not change pinned preview.
- Production entry point exists automatically in both host paths and opens the same surface.
- Preview has no live mutation/effect dispatch path, including keyboard, palette, queued controls and asynchronous callbacks.
- Back-to-live preserves local view state and displays latest record state.
- Stale/resource-mismatched/revoked restore fails without mutation; fresh authorized restore appends history and is undoable and durable.
- App inventory proves coverage beyond Writer, including actual Photos/project previews.
- Rendered wide/narrow UI evidence and meaningful focused tests, then independent task reviews and whole-feature review.

## Coordination

The runtime task owns text layout/performance work. The action task owns shell action integration and recent `record.rs`/`bound_dispatch.rs` changes. Respect their source freeze for the Photos-to-Writer native proof and coordinate shared-file changes.
