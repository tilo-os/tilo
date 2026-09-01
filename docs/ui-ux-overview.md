# qOS UI/UX overview

Status: implementation and acceptance record, 2026-09-01. The issue map records
the starting evidence; the implementation ledger below is the current source of
truth. This does not replace the HIG.

## Executive summary

qOS has strong individual UI work but does not yet present one understandable
product. The largest problems are above the widget level:

1. The visible information architecture does not consistently match the product
   model. `Workspace` and `room`, `People` and `Members`, system permissions and
   workspace permissions, and tabs/windows/apps all compete for the same concepts.
2. The five product-defining journeys are not yet taught as a coherent loop:
   work alone, find/open something, invite someone, understand what they can see,
   then review and revert what changed.
3. The shell gives workspace switching, overview, window tabs, strip focus,
   collaborators, search and system status similar visual weight. A newcomer has
   no clear answer to “where am I?”, “what is shared?”, or “what should I do next?”
4. App quality is broad but uneven. Some screens are composed and legible; others
   are dense dashboards made from nested cards, pills and very small secondary
   text. Narrow layouts sometimes shrink or overlap instead of reprioritising.
5. The toolkit has a good HIG and many shared primitives, but shell chrome still
   contains parallel implementations. That causes behavioural, accessibility and
   visual drift at exactly the surfaces that are supposed to explain qOS.

The first improvement wave should therefore redesign and test the core journeys
and shell hierarchy before polishing the entire app catalogue.

## Implementation ledger

Completed and verified in this pass:

- Public shell copy now consistently uses Workspace, People and Collaborator;
  a pre-commit copy contract prevents the legacy public terms returning.
- The untouched solo top bar has no empty invite/avatar affordance and presence
  is scoped to the active workspace. The shared state reveals the roster only
  when another collaborator is actually present.
- Overview now represents `Workspace -> Strip -> Window group -> Tab`, including
  proportional groups, the active tab and exact collaborator location. Its
  keyboard selection previews before Enter commits.
- Quick Settings root controls use the shared button, toggle and slider faces,
  have explicit keyboard/a11y state, and keep Power reachable at 320 x 568.
- Workspace Settings is a contextual lens rather than a duplicate Settings app:
  wide layouts preserve workspace context, 390 px layouts become an inset sheet,
  all four destinations remain reachable, policy rows stack, and shared buttons,
  segments, avatars and badges provide consistent semantics and focus treatment.
- The canonical collaboration product proof now covers solo work, a durable
  pending invitation, live presence in Notes, Ada-filtered authorship, comparison,
  forward recovery with confirmation, and the same collaborator visibly moving
  to Files. Its executable assertions fail if any frame's claimed state is false.
- Chat now keeps private workspaces quiet, derives People from the active
  workspace only, distinguishes agents, presents an honest view-only composer,
  uses a pushed narrow thread detail, and gates screen-share start while always
  allowing an existing share to stop after a permission downgrade.
- Shared empty states, permission prompts and spinners now have keyboard and
  assistive-technology contracts; permission prompts trap focus and default to
  the safe denial path, while reduced-motion spinners stop animation wakeups.
- Reduced motion also snaps Overview without resetting keyboard preview. The
  shell keyboard/containment contract is recorded in
  `docs/shell-navigation-keyboard-map.md`.
- Settings now distinguishes device policy from contextual workspace access,
  uses a clearer page hierarchy, labels destructive account removal and has a
  tested narrow list/detail path.
- Files has responsive toolbar prioritisation and explicit, localised permanent
  deletion confirmation. Notes has an actionable first-use state. Monitor
  reprioritises its toolbar/table on narrow screens and no longer treats normal
  CPU readings as red/green status. Git Client has readable metadata and quieter,
  semantically stable selection/identity colour. Maps has clearer route-control
  priority, lower categorical-colour load and deterministic search dismissal.

Acceptance outcome:

- Shared UI: 2,235 library tests passed.
- Chat: 187 tests passed, including workspace isolation, read-only behavior,
  narrow threads and screen-share permission downgrade behavior.
- Shell: 1,929 library tests passed with 2 environment-dependent tests ignored;
  the executable collaboration proof passed. Decision-critical desktop and
  390 x 844 renders were reviewed for clipping, attribution, truthful presence
  and recovery feedback.
- No unresolved UX choice currently warrants an experiment. The hard calls were
  answerable from the product contract: contextual Workspace Settings, one
  spatial Overview, quiet solo chrome, and progressive collaboration disclosure.
  Testing two weaker variants would add noise rather than decision value.

## Evidence used

- Product direction in `~/Development/quickdesktop-planning/main.md` and
  `brand-and-launch.md`.
- The current HIG and the hand-rolled component audits.
- Current checked-in shell snapshots and app shots for Settings, Monitor, Maps,
  Calculator and Git Client.
- Read-only inspection of shell overview, top bar, workspace settings, strip and
  collaboration code.

The old top-level `MISSING.md` is useful architectural context but is internally
stale in places. Its claims are not treated as current UI facts unless corroborated
by the current implementation or snapshots.

## Product promise to protect

The planning documents imply a simple experience contract:

- Alone, qOS feels like a calm, capable tiling desktop.
- A workspace is the durable boundary for work, people, permissions and history.
- A strip is cheap layout, never a sharing boundary.
- Collaboration appears when another collaborator exists, without turning the
  whole desktop into groupware.
- Search, actions, history, undo and permissions feel like one system across apps.
- The interface uses stable human vocabulary and does not expose implementation
  terms.

Every UI decision can be evaluated against this contract.

## Issue map

### P0 — Resolve the mental model

#### One public word for the collaboration boundary

The planning brief selects **Workspace**, but the current overview explicitly
models a workspace as a **room** and renders copy such as “New room” and “Nothing
in this room is shared out of it.” This makes a foundational object sound like a
temporary chat location and teaches two names for one boundary.

Decision: keep `room` internal if useful, but remove it from ordinary UI copy,
tests that assert public copy, demo scripts and protocol text shown to people.

#### Stable people vocabulary

Workspace settings use `Members` on wide screens and `People` on narrow screens.
That is not responsive layout; it changes the concept during resize. Use:

- People: settings destination for human membership and roles.
- Collaborator: umbrella term when people and agents can both appear.
- Participant: currently present in a live view or activity.
- Agent: always explicit and visibly typed.
- Peer: networking and diagnostics only.

#### Separate system settings from workspace settings

The standalone Settings app contains “Permissions & Roles,” while the workspace
sheet also owns people, privacy/security, apps and files. Without a strong label,
people cannot predict whether a change applies to their computer, their identity,
one workspace, or one app.

System Settings should own device-, account- and OS-wide policy. The contextual
workspace surface should own collaborators, workspace roles, shared items and app
access for that workspace. Each surface needs its scope in the title.

#### Explain the containment hierarchy

The implementation hierarchy is:

`Workspace -> Strip -> Window group -> Tab`

The current shell exposes several levels simultaneously but does not teach their
relationship. In the overview snapshot the left wheel, top-left “Home 1/3,” tab
row, large workspace card, and strip ribbon all compete as navigation. The tab row
also mixes app names and a generic “Document,” making it hard to tell whether the
user is switching apps, documents or windows.

The resting shell should make workspace identity primary, current strip secondary,
and tab/window controls local to the selected window. Overview should reveal the
whole hierarchy only when requested.

### P0 — Make the defining journey continuous

The first release demonstration is the right product acceptance test:

1. Work alone.
2. Invite a collaborator.
3. See their presence and edit.
4. Open history and understand authorship.
5. Revert the change.
6. See the same collaborator in another native app.

Today these concepts exist across the top bar, collaborator popover, workspace
settings, activity/history surfaces and apps, but they do not read as one journey.
The UI needs one persistent path through them, with designed pending, empty, error
and disconnected states. This should become the canonical usability scenario and
visual regression scenario before more isolated screens are refined.

### P0 — Establish a calmer shell hierarchy

#### Single-person mode is still collaboration-shaped

The planning brief says collaborator-facing affordances stay out of the way until
someone is invited. The shell snapshot still shows a personal avatar/share control,
workspace sharing copy, and presence (“You are here”) in a private workspace. These
are accurate but not useful in the default solo state.

Prefer a quiet workspace title and ordinary window navigation when alone. Reveal
the roster, presence colour and activity emphasis after an invitation exists or
when the person deliberately opens workspace sharing.

#### Overview duplicates navigation instead of explaining the desktop

The overview is a centred modal over a full desktop plus a persistent workspace
wheel. It mostly repeats workspace switching, while the product intent says the
overview should show the complete strip/window arrangement and where collaborators
are. It should be the spatial explanation of qOS, not a second workspace list.

#### Window state controls lose context

In the maximised snapshot, “Restore” and close float near the screen centre while
the app label sits at the far left. The controls do not visually belong to the
window they affect. Window state must remain attached to the window-group frame,
including during maximisation and hover reveal.

#### Too many pill-shaped selectors

Workspace status, tabs, scope filters, app modes, display units and many actions
all use pills. This weakens their meaning. Reserve the fully rounded selector for
mutually exclusive choice or compact identity/status; use ordinary rows and buttons
for navigation and actions.

### P1 — Repair visual hierarchy and density

#### Nested cards dominate content

Settings and Monitor use a card for the page header, cards for groups, cards for
metrics, bordered controls inside those cards, and a dark page ground beneath all
of them. The repetition adds edges without adding structure. It is particularly
heavy in Settings, where rows that belong to one list become large nested panels.

Use whitespace, type and alignment for primary grouping. Use a raised card only
when the content is genuinely movable, selectable, temporary or distinct from its
surroundings.

#### Secondary text is frequently too dim and too small

Captions, metadata, table columns and instructions often use low-contrast blue-grey
at small sizes. This is visible in the workspace overview, Settings, Monitor and Git
Client shots. The information may technically be present but is not comfortably
readable at laptop scale.

Define and test a minimum body/caption size and contrast for shell and app content.
Do not use dim colour as the only way to create hierarchy.

#### The shell and apps do not yet share one visual voice

The shell is dark graphite with floating translucent chrome, Settings is a card
stack, Monitor is an instrumentation dashboard, Maps is a dense overlay, and Git
Client is a compact developer tool. Domain differences are healthy, but spacing,
type scale, control height, selection weight and surface usage should be recognisably
the same system.

The website's planned radical monochrome direction should inform a deliberate shell
refresh, but should not be copied literally until interaction states and contrast
are proven in light, dark and high-contrast themes.

### P1 — Make responsive layouts reprioritise

The HIG correctly distinguishes sidebars, overview/detail content and immersive
surfaces. Current narrow snapshots show remaining problems:

- Monitor's search field and Tree/Sum subtree controls overlap at narrow width.
- Its process table requires a horizontal scrollbar and loses useful columns,
  instead of switching to a compact per-process row/detail pattern.
- Settings opens a large navigation drawer that consumes most of the viewport;
  the underlying page has little remaining context and the dismissal affordance
  is not obvious in a still.
- Several desktop-scale toolbars retain all modes and secondary controls rather
  than prioritising one action and moving the rest into overflow.

Add minimum-width screenshot scenarios for every layout class, not merely every app.
The required widths should include at least phone portrait, small tablet/half-screen,
standard laptop and large desktop.

### P1 — Finish shared shell primitives

The component-unification audit identifies the most consequential remaining debt:

- Workspace settings still combines custom modal, tab, field, button, avatar and
  badge behaviour.
- Quick Settings retains custom tiles/sliders and several row patterns.
- Collaboration avatars/presence clusters are repeated across shell surfaces.
- Chips, badges and scattered action buttons still have parallel paint/hit paths.

This is not only cosmetic. Parallel implementations lose focus, keyboard operation,
accessibility roles, IME behaviour and state consistency. Complete these migrations
as part of redesigning the affected journey, rather than as an isolated LOC-reduction
project.

### P1 — Accessibility and localisation are release UX

The repository still calls out incomplete AT-SPI/accessibility integration and
hard-coded English in some shell/identity surfaces. A system based on semantic actions
cannot treat its own UI as the exception.

Every redesigned core journey needs:

- keyboard-only completion and visible focus;
- screen-reader names, roles, state and live-region behaviour;
- 200% text/display scaling without clipping;
- light, dark and high-contrast captures;
- strings routed through Fluent with enough room for expansion;
- reduced-motion behaviour for spatial transitions.

### P2 — Rationalise the app portfolio

Forty-plus apps are valuable toolkit stress tests, but they create a misleading sense
that every app deserves equal polish now. Define tiers:

1. **Product proof:** Files, Notes/document editing, Search, Settings, Chat/presence,
   History/activity.
2. **Daily-driver core:** Browser, Terminal, Mail, Calendar, media and system tools.
3. **Toolkit probes:** specialist or ambitious apps that may remain explicitly
   experimental.

Apply the full visual, responsive, accessibility and empty/error-state bar to tier 1
first. Label experimental apps honestly rather than presenting an uneven catalogue as
uniformly finished.

### P2 — Design zero, loading, error and permission states as a system

The HIG asks for honest states, but the current repository still contains bespoke
placeholder, unavailable, waiting and permission copy. Create shared state patterns for:

- empty but ready;
- first-use guidance;
- loading with known progress vs indeterminate waiting;
- offline/disconnected collaborator;
- permission required, denied and expired;
- application failed to start or stopped responding;
- no results and filtered-to-zero results.

Each state should say what happened, whether work is safe, and the next available action.

## Screen-specific findings from the current shots

### Shell overview

- Public `room` vocabulary conflicts with the chosen `workspace` vocabulary.
- “You are here” and private-sharing explanations over-explain the solo state.
- The active workspace is indicated in the wheel, top bar and card at once.
- The overview does not show the promised strip/window arrangement.
- Main content is heavily dimmed, so the spatial relationship to the desktop is lost.
- “New room” is low emphasis and easy to miss despite being the only creation action.

### Settings

- The hierarchy is dominated by bordered cards inside a bordered page.
- External account types, individual accounts and account actions are not separated
  strongly enough by type scale or spacing.
- Delete is an unlabeled icon with the same weight as Edit; destructive intent is weak.
- System-wide “Permissions & Roles” competes with contextual workspace permissions.
- The narrow drawer needs a clearer close/back gesture and a more compact destination
  strategy.

### Monitor

- Wide layout is information-rich, but nearly every region is boxed.
- At narrow width, filters overlap and the table degrades into clipped columns plus a
  horizontal scrollbar.
- Red/green CPU values rely heavily on colour and use alarm-like emphasis even when the
  person has not defined a threshold.
- The process detail/kill area is useful on wide screens but needs a push/sheet pattern
  on narrow screens.

### Maps

- The full-bleed model is correct and the route panel is substantially more coherent
  than many app screens.
- The panel is dense and uses multiple stacked segmented controls, causing all choices
  to look equally primary.
- Search results, saved places and recent searches become one very tall overlay; section
  priority and dismissal need stronger treatment.
- Purple action colour, red route endpoints, orange/blue/green POIs and map geometry
  create a high categorical-colour load.

### Git Client

- The domain-appropriate density is good, but text is very small and metadata contrast
  is weak.
- Selected rows use a large saturated block that overwhelms nearby content.
- `Syn` is unclear copy and likely a truncation/abbreviation failure.
- Author colour changes meaning between avatar, author text, branch graph and diff state.

## Recommended order of work

### Wave 1 — Product model and shell skeleton

1. Freeze the public vocabulary and add a copy lint for forbidden terms.
2. Redesign the resting single-person shell.
3. Redesign overview around the actual workspace/strip/window arrangement.
4. Attach window controls unambiguously to their window group.
5. Define the shell's navigation and keyboard map in one diagram/spec.

### Wave 2 — Collaboration proof

1. Build the invite -> present -> edit -> history -> revert journey as one scenario.
2. Unify workspace people, permissions, activity and sharing surfaces.
3. Cover pending/offline/denied/revoked/error states.
4. Verify the same journey in a second native app.

### Wave 3 — System foundations

1. Complete shell adoption of shared interactive primitives.
2. Lock typography, spacing, surface and responsive-layout contracts in the toolkit.
3. Add accessibility, localisation, scale and contrast acceptance tests.
4. Establish shared empty/loading/error/permission patterns.

### Wave 4 — App convergence

1. Polish the product-proof app tier end to end.
2. Fix responsive failures by layout class.
3. Converge daily-driver apps on the same hierarchy and primitives.
4. Keep toolkit probes explicitly experimental until they meet the bar.

## Suggested acceptance metrics

- A first-time tester can explain workspace vs strip vs tab after five minutes.
- A tester can invite someone and state what will be shared before confirming.
- A tester can identify who changed something and revert it without instruction.
- No collaborator-specific chrome is prominent in an untouched solo workspace.
- Every P0 journey works with pointer, keyboard and screen reader.
- No overlap or clipped primary action at the supported minimum widths.
- Every public concept has one stable label across viewport sizes.
- Visual regression coverage exists for light, dark, high contrast, 200% scale and
  reduced motion on the core journeys.

## Immediate next design task

Create two annotated shell wireframes before editing widgets:

1. **Solo resting state:** one workspace, one strip, one window; no collaborators.
2. **Shared active state:** one collaborator present, with the route to people,
   permissions and activity visible.

Those two states force the central qOS promise into visible decisions and give later
component work a target instead of merely making the current hierarchy prettier.
