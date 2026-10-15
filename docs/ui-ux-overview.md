# Tilo UI/UX release ledger

Status: active implementation ledger, updated 2026-09-05.

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
- A production-fresh workspace has a visible, keyboard- and assistive-
  technology-accessible “Open an app” route into the Applications palette; it
  no longer relies on knowing the `Super+N` shortcut.
- Shared empty-state actions use one semantic action path for pointer,
  keyboard, and accessibility activation. Closed dialogs remove their hidden
  descendants from both focus traversal and the accessibility tree.
- First-login completion keeps its desktop exit action outside the scrolling
  content region, including the supported laptop viewport at 130% text scale.
- Settings category chrome uses theme-driven vector icons instead of optional
  private-use font glyphs, so its navigation remains legible in deterministic
  rendering and minimal installations.
- Chat has exactly two contextual destinations: the active Tilo Workspace and a
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
- Shell overlays now remain frame-driven through their complete transition,
  including reduced-motion mode. Notifications wake the compositor only for
  meaningful animation or expiry deadlines, and narrow layouts constrain their
  width instead of overflowing.
- Contacts and Mail use the shared empty-state language and interaction model.
  Contacts presents first run as one app-wide state, with one primary action;
  Mail distinguishes no account, pending sync, empty folder and empty search.
- A source audit ratchets hard-coded colour escapes across apps and shell so new
  theme debt fails tests instead of silently increasing.

## Capability honesty

- Remote Desktop offers SSH, VNC, and Tilo device connections. Tilo sessions use
  the shell's authenticated exact-target device route, stay waiting until the
  target shares a workspace surface, and fail closed on unavailable relays,
  denial, revocation, or route loss. Existing RDP profiles remain
  preserved and editable, but visibly unavailable.
- Device pairing is manual-copy today: the screen asks for the code and pairing
  payload supplied by the existing device. It does not advertise a scanner that
  is absent.
- Production does not advertise screen sharing while the compositor lacks a
  consuming PipeWire producer. Native and hosted paths fail provisioning
  explicitly; synthetic nodes are confined to tests. Existing defensive receive
  and stop handling remains available for future integration.
- Calendar's visible CalDAV operations use the live queued PUT/DELETE and pull
  paths. Music and Tube's visible streaming/playback paths use their production
  backends; cross-device library synchronisation is a separate release gate, not
  implied by those controls.
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
   supported desktop stack. The installed-image VM now fails unless an external
   AT-SPI client can read and drive the compositor-owned OOBE, desktop and
   protected lock screen. It also launches the signed installed Files and
   Settings payloads, finds them through the real AT-SPI bus, verifies their
   named roots and meaningful controls, invokes a safe action, and observes the
   resulting tree state. That executable evidence does not substitute for a
   recorded screen-reader, 200% scale, high-contrast, hardware, service, or
   full product-journey acceptance run; monolithic custom-painted apps remain
   on the app-by-app semantic/action conversion track.
2. **Localisation.** Public shell and app strings need Fluent ownership and
   expansion-safe layouts. English literals are not a release-ready fallback.
3. **Standard-app journey acceptance.** The visible-control audit is complete,
   but each promised standard app still needs a driven, product-shaped journey
   before release-surface promotion. Office breadth and Music/Tube cross-device
   library sync are the main unproven capabilities; experimental developer tools
   must remain clearly separated from standard apps.
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
- Tilo does not add a second encryption/governance stack for this. Existing
  signed workspace authority remains the source of truth; future content-key
  rotation is a separate confidentiality feature, not a prerequisite for
  correct membership enforcement.

## Acceptance discipline

- Drive the real interaction; an empty-state screenshot is not evidence that an
  action works.
- Keep deterministic test data and pin time/network inputs before visual
  comparison.
- Render replacement goldens with `t outputs visual-baselines`, which downloads
  the candidates to `target/visual-baseline-review`. Inspect the changed PNGs
  and diff inventory, verify the patch checksum, and apply only accepted paths.
  Commit that review explicitly, then let nightly compare the
  result without update variables. CI must never silently commit a baseline.
- Document scope, source of truth and verification beside every release-affecting
  change. Do not treat an implementation plan as a shipped capability.

## Source documents

- Product direction: [`docs/product.md`](product.md).
- Interaction and component rules: [`crates/sdk/ui-core/HIG.md`](../crates/sdk/ui-core/HIG.md).
- Shell keyboard/containment map:
  [`docs/shell-navigation-keyboard-map.md`](shell-navigation-keyboard-map.md).
- Current executable UI evidence: shell `shots` scenarios and
  `crates/session/shell/tests/e2e_shell_flows.rs`.
