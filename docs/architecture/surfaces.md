# Surfaces

Anything the compositor paints *over* the desktop is a **surface**: a menu, a
sheet, a picker, a question, a toast. A surface is described once — its kind,
where it stands, who may use it, who owns it, where it is in its life — and
everything else follows from that description. The host does not ask "which
overlay is this?" to decide how input reaches it, what it makes inert or what
assistive technology hears; it reads the surface.

The vocabulary is `crates/session/shell/src/shell/overlay_host/layers.rs`
(`Role`, `Anchor`, `Modality`, `Owner`, `Material`, `Lifecycle`, `Surface`), and
`ShellLayer::surface` is how a component answers with it.

## Why one model

Today each overlay re-derives the same decisions: `LayerId::band` is a lookup
table of hand-assigned bands, `OverlayHost` routes the overlays that never
joined `OverlayStack` itself, `scrim` is drawn per overlay, and whether the
keyboard lands inside a menu is remembered in the component that opened it.
Every one of those is a property of *what the thing is*, so it has one answer:
a component that says it is a `Role::Consent` on `Modality::SystemModal`
answers a question for the whole system, not for itself.

The model also makes the surface an app's public asking path. An app does not
draw a picker; it exports a resource action that raises a surface owned by
`Owner::App`, and its outcome returns through the same action. That is what
automation, accessibility and agents see — the same door a person uses.

## Kinds

| `Role` | What it is | Who raises it | Examples today |
| --- | --- | --- | --- |
| `Popup` | A menu or flyout hanging beside what it acts on | chrome or a context | Power menu, tab context menu, window menu |
| `Overlay` | A mode the person is held in until they answer it | the person's chord | Alt+Tab switcher, emoji picker, dictation, command palette, overview |
| `Dialog` | A sheet a person asked for and works in | the person, or an app | Quick Settings, workspace settings, display layouts, collaborators, now playing, shortcut help |
| `Picker` | A picker raised for another process, which is waiting | a portal request | file chooser, screenshot/screencast selection |
| `Consent` | A question that *is* the grant | the system only | display change Keep/Revert, permission prompts, pairing and invite questions |
| `Toast` | A short, self-dismissing message | anything | warnings and confirmations |

The kind fixes everything below, so a new overlay picks a kind and inherits the
rules instead of inventing them.

## Placement

A surface names an `Anchor` and the host resolves the *display* and the rect:

- `Center` — centred on the focused display, inset from every edge.
- `TopTrailing(margin)` / `TopLeading(margin)` — hanging from the top bar.
- `BottomCenter(caret)` — the dock's own edge, or above a caret it must not cover.
- `Beside { at, gap }` — beside the rect it was opened on: the tab, the menu
  button, the caret. The side with room wins, and the surface never overlaps
  `at`.
- `At(point)` — over the pointer, for a context menu.
- `Fill` — the whole display.

`Anchor::display_of` is the only thing that chooses a display: the one holding
the anchor. A surface with no anchor rect lands on the **focused display** — the
one whose windows take the keyboard — and never on the primary or on the whole
desktop, because on a docked laptop the desktop viewport is all of it. This
rule is today's `LayerGeometry::display_for`, kept and made the model's.

## Modality

`Modality` says who may work while a surface is up:

- `Passive` — the desktop stays usable; what the surface ignores goes on to the
  layer below. Now playing, presence markers, a readout.
- `WindowModal` — everything behind is inert while it is up. This covers the
  overlays people expect to be stuck in (a menu, a picker, the switcher) and
  does not carry the system-wide weight of a consent question.
- `SystemModal` — nothing behind is reachable and nothing can dismiss it by
  accident; the session is waiting. The display change question, first-use
  setup, a portal consent prompt.

Modality decides the **band** input is offered in, so the bands stop being a
hand-maintained table:

| `Modality` | `Band` |
| --- | --- |
| `SystemModal` | `Session` |
| `WindowModal` | `Modal` |
| `Passive` | `Popover` |

Within a band, `LayerId` order still decides, because a component problem is
not a kind problem: Alt+Tab and dictation both answer to the held-modifier edge
of the dispatch, whichever kind they are.

**The scrim is not the modality.** A modal surface makes inert *its own display
for a `WindowModal`, and every display for a `SystemModal`* — a question about
one display does not freeze the others, but an answer the session waits on does.
`Surface::scrim` is the single source, and the host paints it and drops the
input behind it from the same rect, so what looks covered is covered.

## Focus

- **Initial focus**, by kind: a `Popup` and a `Dialog` focus their first
  actionable row; an `Overlay` takes the keyboard inside itself; a
  `Consent` focuses the safe answer — Revert over Keep, Deny over Allow — so
  Enter on an unattended prompt does the recoverable thing.
- **Trap**: a `SystemModal` surface keeps `focus` inside itself for the
  keyboard and for the accessibility tree. A `WindowModal` one keeps the
  *destination* focus — the field the caret is in, the tab the menu is about —
  and returns it on close.
- **Restore**: closing a surface returns focus to whatever held it when the
  surface opened — the window, the field, the tab — recorded at open time, not
  re-derived at close.
- A `Passive` surface never takes focus, and never traps it.
- A surface opened while another is up stacks on it and gets focus; the one
  below keeps its state and its focus record, so a chain of popups unwinds.

## Dismissal

| Cause | `Popup` | `Overlay` | `Dialog` | `Picker` | `Consent` | `Toast` |
| --- | --- | --- | --- | --- | --- | --- |
| Escape | closes | answers or closes if its own model says so | closes | cancels the request | *never* — it answers, its owner cancels | n/a |
| Press outside | closes | stays | stays (`SystemModal` dismisses only through its own controls) | cancels | never | n/a |
| Action on it | closes | answers | closes | answers | answers | n/a |
| Its subject closes | closes | stays | stays | cancels | stays until the transaction settles | n/a |

A `SystemModal` surface never closes as a side effect: it closes because it was
answered, or because the thing it asks about settled on its own. That is why
`DisplayConfirmPrompt` reverts rather than disappears when its deadline passes.

## Ownership and lifecycle

`Owner` is `Shell`, `App(..)` or `Portal(..)`, and it decides three things: who
is told about the outcome, whose presence and permission scope it is counted
in, and what it may do at all. A `Consent` surface is always `Owner::Shell` in
the sense that matters — it is asked by the system *about* an app or a portal
caller, and never raised on that caller's word.

`Lifecycle` is `Requested`, `Shown`, `Closing`, `Closed`. A `Consent` surface
sits in `Requested` long enough for the caller to be told what is being asked
(the portal's request must not time out while the person reads); it shows no
content before `Shown`, and it takes no input outside it. `Closing` is the exit
motion: the surface keeps painting at the place and size it will animate out
of, so its exit never pops from a stale geometry. `Closed` is where it releases
focus back to whatever `Focus` recorded.

The component owns this state. The host owns only the stack: the order surfaces
are offered input in, the scrim, the paint order and the frame demand. A
component that needs something beyond itself asks for it with a `LayerIntent`
and never reaches into the host.

## Accessibility

- **Role from kind**: a `Popup` is a menu, an `Overlay` a dialog or group
  depending on whether it holds the keyboard, a `Dialog` a dialog, a `Picker` a
  dialog named for what it picks, a `Consent` an alert dialog, a `Toast` a
  status. Nothing declares its role by hand.
- **Name** is the surface's own: the menu's name carries its captions; the
  prompt's name is the question. Not the widget type and not a translation key.
- **Trap**: a modal surface publishes the modal trait, so a screen reader does
  not wander behind it. A `Passive` surface publishes nothing of the sort.
- **Order**: the stack's order is the reading order, topmost first, and a
  layer's index among the open ones is its semantic path root. That stays.
- **A surface a component implements is in the tree or it is not there at all**
  — a layer that says it is open and publishes no accessible node is a defect,
  not a speedup (`LayerId::DisplayConfirmation` was one).

## Stacking, animation and frames

Stacking is `LayerId` order, topmost first, and it is the order input, reading
order and Escape already use. It is not derived from kinds: a display question
answered above onboarding is a fact about the session, not about either one's
modality, which is why `Band` stays an explicit ordering and only its *values*
come from the model.

Exit animation belongs to the surface: a surface in `Closing` keeps asking for
frames through the stack's frame demand. A surface that is `Closed` demands
none.

## The forms the acceptance names

- **Window** — a tab in the tiling tree. It is not a surface: it never overlaps,
  and its kind, placement and focus are `Workspace → Strip → WindowGroup → Tab`.
- **Popover** — `Role::Popup`, anchored `Beside` or `At`, `WindowModal`,
  typically `Material::Floating`.
- **Sheet** — `Role::Dialog`, `Anchor::Center`, `SystemModal` when it holds the
  session, `Material::Floating`.
- **Fullscreen** — `Anchor::Fill`. A mode over the desktop (the switcher,
  the overview, the display question), never a window resized to fill.
- **Per-display surfaces** — anything anchored to a display's edge is per
  display by construction (`Anchor::display_of`); a surface drawn once for the
  whole desktop says `Anchor::Fill`.

## Migration

The stack is the migration's order:

1. **This step — the model.** The vocabulary and the contract above exist;
   every layer of `LayerId` declares the surface it is (`LayerId::surface_kind`),
   and the six that implement `ShellLayer` answer from their own state
   (`ShellLayer::surface`). `OverlayStack` can be asked the surface of any
   `LayerId`, which open surfaces exist, and what scrim is up. Tests hold the
   declaration to the host's routing: a layer whose modality maps to a band
   must be routed in that band.
2. **Input from the model.** `OverlayStack` offers input to `Band::of` rather
   than `LayerId::band`, and `OverlayHost`'s own routing uses the same answer,
   so the table and the match arms stop disagreeing.
3. **Ownership by layer.** The remaining overlays join `OverlayStack` and
   answer `ShellLayer::surface`; the host's per-overlay `match` sites shrink to
   the few that are genuinely host policy (band order, drag, portals).
4. **Scrim and focus from the surface.** The host paints `OverlayStack::scrim`
   and restores focus from the surface's record instead of each overlay
   remembering its own opener.
5. **Portals and consents** say `Owner::Portal(..)` / `Owner::Consent` and use
   `Lifecycle::Requested`, so a caller is told what is being asked before the
   person sees it.

Each step lands on its own and keeps the desktop's behaviour identical; the
model is reached by removing the special cases, not by a rewrite.
