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
- Chat has exactly two contextual destinations: the active qOS Workspace and a
  system Inbox that groups cross-workspace messages by their source. Chat no
  longer creates a second, contradictory kind of "workspace".
- Git Client, Notes, Monitor and Maps expose their primary controls through the
  assistive semantic tree, route semantic activation through the same hit
  targets as pointer input, and preserve normal Tab navigation.
- Collaboration invitations are signed, independently correlated credentials.
  Pending invitations survive restart in private local storage and can be
  resent, canceled or allowed to expire from Workspace Settings.
- Shared workspaces replicate their strips and windows. Focus remains personal;
  Follow is the explicit opt-in that also drives supported app viewport and
  view-mode state, and direct interaction stops Follow.

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

1. **Accessibility acceptance.** Core journeys still require recorded
   screen-reader, 200% scale and high-contrast acceptance evidence on the
   supported desktop stack. The semantic and keyboard implementation is no
   longer the blocker.
2. **Localisation.** Public shell and app strings need Fluent ownership and
   expansion-safe layouts. English literals are not a release-ready fallback.
3. **App capability audit.** Every visible control must either operate on a
   production backend, be clearly presented as unavailable, or be removed.
   Experimental apps need an explicit experimental label rather than the same
   implied readiness as product-proof apps.
4. **Responsive coverage.** Product-proof journeys require visual and driven
   acceptance at phone portrait, small tablet/half-screen, laptop and large
   desktop widths.

## Collaboration revocation contract

- Canceling a pending invitation prevents that credential from admitting a new
  member. Invitations are independent, so canceling one does not affect another
  invitation for the same workspace.
- Removing a member takes effect at the authoritative current-membership check:
  later sync, presence and signed document operations from that identity are
  rejected, including operations delivered after an offline period.
- Revocation is not retroactive deletion. Data already downloaded to another
  device cannot be clawed back, and the UI says so plainly.
- qOS does not add a second encryption/governance stack for this. Existing
  signed workspace authority remains the source of truth; future content-key
  rotation is a separate confidentiality feature, not a prerequisite for
  correct membership enforcement.

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
