# qOS UI/UX release ledger

Status: active implementation ledger, updated 2026-09-01.

This document describes the current UX bar and the release-relevant work that
is actually verified in the tree. It is not a backlog, a design diary, or a
substitute for the HIG. Historical audits live in Git history; do not use them
to infer present product behaviour.

## Product contract

- A **Workspace** is the durable boundary for work, people, permissions and
  history.
- A **Strip** is layout within a workspace, never a sharing boundary.
- A **Window group** holds tabs. Window controls belong to that group.
- Collaboration is progressive: a solo workspace is calm; people, activity and
  sharing appear when they are relevant.
- Product UI uses `Workspace`, `People`, and `Collaborator`. `room` and `peer`
  are implementation terms, not public vocabulary.

## Current, verified UX baseline

- The shell has a quiet solo state and contextual collaboration chrome rather
  than permanent sharing decoration.
- Overview explains the `Workspace → Strip → Window group → Tab` hierarchy and
  supports keyboard preview before committing a selection.
- Workspace Settings is contextual to the active workspace; system Settings is
  device/account scoped. Narrow layouts use a contained sheet rather than a
  desktop dialog squeezed into a phone-sized view.
- The core collaboration proof covers authored activity, comparison, recovery
  feedback and visible cross-app presence. It is executable rather than a
  screenshot-only claim.
- Shared empty states, permission prompts, keyboard focus and reduced-motion
  behaviour are available in the redesigned shell journeys.
- Files, Notes, Settings, Monitor, Maps and Git Client have targeted hierarchy
  and narrow-layout improvements. This does **not** mean their full release
  acceptance is complete; see the gates below.

## Capability honesty

- Remote Desktop offers only SSH and VNC when creating a connection. Existing
  RDP and qOS Relay profiles are preserved and editable, but visibly
  unavailable; qOS does not present a dead Connect action.
- Device pairing is manual-copy today: the screen asks for the code and pairing
  payload supplied by the existing device. It does not advertise a scanner that
  is absent.
- Golden-image snapshots may seed locally for deliberate first-time work.
  Continuous integration requires an existing checked-in golden. End-to-end
  shell captures decode pixels, check for rendered content, and assert visible
  change for state-changing flows; settled decision states use goldens.

## Release gates still requiring evidence

These are gates, not a claim that every item is blocked by the same subsystem.
An item leaves this list only with an executable test or an inspectable manual
acceptance record.

1. **Collaboration truth.** An invitation must create durable, authenticated
   pending state and be accepted through the production transport—not by a shot
   fixture. The product must define and implement member/device revocation,
   including what immediately stops, what historical data remains readable and
   how keys rotate.
2. **Accessibility parity.** Chat, Git Client, Notes, Maps and Monitor need
   complete semantic trees and keyboard equivalents for their primary actions.
   Core journeys require screen-reader, focus, 200% scale and high-contrast
   acceptance evidence.
3. **Localisation.** Public shell and app strings need Fluent ownership and
   expansion-safe layouts. English literals are not a release-ready fallback.
4. **App capability audit.** Every visible control must either operate on a
   production backend, be clearly presented as unavailable, or be removed.
   Experimental apps need an explicit experimental label rather than the same
   implied readiness as product-proof apps.
5. **Responsive coverage.** Product-proof journeys require visual and driven
   acceptance at phone portrait, small tablet/half-screen, laptop and large
   desktop widths.

## Acceptance discipline

- Drive the real interaction; an empty-state screenshot is not evidence that an
  action works.
- Keep deterministic test data and pin time/network inputs before visual
  comparison.
- Refresh a golden deliberately with `UPDATE_SNAPSHOTS=1`, review it, commit it,
  then run without the variable to compare it. CI must never silently create a
  baseline.
- Document scope, source of truth and verification beside every release-affecting
  change. Do not treat an implementation plan as a shipped capability.

## Source documents

- Product direction: `~/Development/quickdesktop-planning/`.
- Interaction and component rules: [`ui/HIG.md`](../ui/HIG.md).
- Shell keyboard/containment map:
  [`docs/shell-navigation-keyboard-map.md`](shell-navigation-keyboard-map.md).
- Current executable UI evidence: shell `shots` scenarios and
  `shell/tests/e2e_shell_flows.rs`.
