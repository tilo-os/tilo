# Search and History views: what they show and why

The mechanics are already specified: [search.md](architecture/search.md) describes providers,
resources and actions, and [history-experience.md](history-experience.md) the action record
and its three zoom levels. This note covers what a person *sees*: what the palette shows before
anyone types, what wins when they do, and what counts as history. Each rule below exists
because a user-level audit (September 2026, headless, en/de, light/dark) found it broken.

## What the audit found

Search (Super+Space, fresh session, then with Notes and Files open):

- **Enter on a window closes it.** With Notes open, typing "notes" put *Close tab “Notes”*
  first. Open windows are only listed as close, move and copy rows; there is no "go to" row.
- **Raw IDs in subtitles and matching.** Window rows read `App so.tilo.notes · strip “Main” ·
  group 10797098631644053512 · tab …`, and the ranker matches those IDs. Host apps read
  `Flatpak · unconfined host app · com.onepassword.OnePassword`.
- **An empty query is an inventory, not a starting point.** The first four registered actions
  (help, tour, overview, settings), then every Flatpak on the machine alphabetically, with
  "105 results" underneath. Once windows are open, *Close tab* rows join the default list.
- **Wrong rows for simple words.** "history" / "verlauf" returns *Switch to Home*, because every
  workspace view shares the keywords "workspace activity history". Those rows all say "Local
  workspace view". Typing "h" puts *Pair a home server* first.
- **No undo, restore or theme.** "undo", "rückgängig" and "dark" return nothing.
- **Bookkeeping in results.** *Disable Notes search* (showing the source ID `notes`) appears for
  "notes"; *Keep/Revert display settings* and *Decline Bluetooth pairing* are listed when
  nothing is pending.
- **Language gaps.** In German, the footer ("Navigate · Open · Close · N results") stays
  English, and an English query ("workspace activity") finds only config files.
- **Filter noise.** Two rows of 13 chips, including kinds with no results, and "Host Apps" vs
  "Applications", which is how the implementation is built rather than how people think.

History (Super+H, fresh session, then the shots fixtures):

- **Your own setup presented as news.** On a brand-new desktop Catch up says “While you were
  away · <your name> kept things moving”: *Updated workspace access*, *Recorded workspace “Home”*.
  Those are the bootstrap writes of the workspace you just created.
- **Opening something counts as a change.** “6 changes” includes *Opened Q3 budget*,
  *Opened Retro*. Views are not edits.
- **A summary that restates the list.** “At a glance: Work centered on A and B.” is the two
  card titles again, and is hard-coded English (as are “N changes over 11 d”, “named by”,
  “1 action”, and the ALL/EDITS/COMMENTS/GOVERNANCE/DELETES tabs) in the German UI.
- **Explore opens with five rows of controls** (view, time, people, layout, action family)
  before the first entry. There are no time headings, and every row repeats “just now”.
- **Per-app History is a different surface.** Notes' own History tab does not share the
  workspace view's rows, diff or undo.

Live snapshots of the headless compositor go white as soon as any tile is open; the audit's
visuals therefore come from `shots`. That belongs to the rendering owner.

## Palette information architecture

**One door.** Super+Space and a bare Super tap open the same palette, scoped
to the workspace in front, with one control in the field (the scope token:
click it or press Backspace) that widens it to every workspace. The tap
opens it as the Super surface, over a light scrim with the desk widgets
open around it and the time and status; holding Super shows the field
alone, a glance at the widgets. Ctrl+K opens it scoped to the focused app.

### Empty query: the launcher

With nothing typed, global search is the launcher
(`crates/session/shell/src/shell/search/launcher.rs`), top to bottom, each
section left out when it has nothing real to show:

1. **Happening now**: per workspace with someone in it, a row wearing their
   faces, then a row per window someone is in (Enter goes there) and per
   call in progress (Enter joins it). Read from the same presence the strip
   and the window bar use.
2. **Waiting on you**: calls ringing, messages and mentions, and questions
   addressed to the person, from the notifications the desktop holds; Enter
   does what the notification's own default action does. One sent from no
   workspace (a call to the person, not to a room) shows in every scope.
3. **Continue**: up to six places the person's windows were lately, newest
   first: each window's location as its app declares it to the window bar,
   reopening at the view's anchor when the app gives one.
4. **Apps**: every app the person has (installed Tilo apps and the
   device's own), the ones used most first, then grouped by what they are
   for. Apps the catalogue offers to get are found by typing, never listed
   here.

Every row says why it is there in its second line. A short card never
trims the first three sections (their providers answer with only their
few); the apps share the rows left, each purpose cut short offering the
rest in place. Each section is a
search provider answering the empty query
(`SearchProvider::launcher_section`), and an empty `actions_enumerate`
answers with the same sections under `launcher`.

### Typed query: ranking

The merge keeps today's lexical strength plus provider score, with these additional rules:

- **Going somewhere beats changing something.** For the same subject, a view/go-to row outranks
  an effect row. Destructive rows need their verb in the query ("close notes",
  "schließen") or the Actions filter.
- **Match what people can read.** Titles, subtitles and localized keywords match, with the
  English keywords as a fallback for every locale. Internal IDs (group, tab, app IDs, provider
  IDs) never match, except when pasted in full.
- **Transient and meta rows need a subject.** An action whose subject is absent is not listed;
  "disable … search" only matches "search"/"disable"/"suche".

### Groups and rows

Groups are ordered by their best match and then by this fixed order: **Windows · Apps ·
Resources** (one group per resource type, drawn by the app's renderer) **· Files · Actions ·
Settings · People**. Native and Linux apps share one *Apps* group and one *Apps* filter; each Linux app's row
states which kind of app it is (Flatpak, AppImage, program or the system's) as a fact, the
same answer Files and `tilo app list` give. Store pages of third-party apps the person does
not have are their own section, *In the Store*, after everything the person has, and the
*Apps* filter keeps them;
Tilo apps that open at once stay under *Apps*. No two groups share a heading. An open app is
two kinds of row that rank together: the app, which starts it (a new window), and each of its
windows, which switches to that window; Shift on a window opens a new window of its app here.

A row shows **what** (title), **where** (app · workspace, path, or owner) and at most one
live badge. Subtitles never show IDs.

### Keyboard

`↑/↓` move, `Enter` runs the primary action (open, go to, run), `Ctrl+Enter` opens
beside, `→` or `Tab` lists the row's other actions (close, move to workspace, show history,
reveal in Files), `←` goes back, `Esc` closes. Filters are `Alt+1…9`, and only kinds that have
results appear.

## History information architecture

History answers three questions, each a filter over the same action record:

| Question | Where | Scope |
|---|---|---|
| What changed while I was away? | Catch up (Super+H) | this workspace, since my seen frontier, others' work |
| What happened to this thing? | *History* on any resource (in its app, in Files, from search) | one resource and its children |
| What did this person or agent do? | Clicking an avatar or chip | one collaborator in this workspace |

**What counts as history:** content changes to resources, membership and permission
changes, and undo/restore. **What does not:** opening or viewing, window layout, and the
workspace's own bootstrap writes (initial owner, first record). Permission changes stay visible
under their own *Access* lens, and are never presented as "while you were away" when you made
them.

**Catch up** shows others' and agents' work since you last looked, as outcome cards. It
never shows your own work, and has an honest empty state: “Nothing new since you last
looked”, with *Show all history*. There is no generated sentence unless it adds something the
cards do not say, and any generated sentence is labelled as a summary.

**Timeline** (the explorer) starts with one filter row: search, then People, Apps,
Kind and Time as menus. Entries sit under **Today / Yesterday / This week / Earlier** headings.
A row reads *avatar · who · verb + resource · app · time*. An agent row names the agent and the
person it acts for. Selecting a row opens detail: the diff (via the resource type's
renderer), *Open at this point*, *Restore this version*, and *Undo this change*. Undo and
restore append inverse actions; history is never rewritten. Replay stays reachable from the
view menu, not as a peer tab.

**Per-resource history is the same component.** An app's History tab and Files' History pane
embed the timeline with the resource filter applied, so rows, diff and undo match
everywhere.

## Actions

Every capability above is an action, so search, accessibility and agents reach it without
clicking:

- windows: `tab.focus:{group}:{tab}` next to the existing close, move and copy rows;
- history: `room.activity.open` (catch up), `room.activity.explore` with optional
  `person`, `app`, `resource` and `since` arguments, `history.compare`, `history.undo`, and
  `history.restore` on an entry or group;
- search surfaces "undo", "restore" and "history of …" as those actions for the focused
  resource.

## Delivery

Small PRs, each with production-path tests:

1. This note.
2. Window rows: *Go to* first; close and move need their verb; readable subtitles; no ID matching.
3. Row hygiene: per-row keywords and subtitles for workspace views, meta and transient rows
   only when meaningful, translated footer.
4. The launcher for the empty query.
5. Activity: localize the `ui` history strings, drop the restated summary, and keep bootstrap
   writes and views out of Catch up and out of change counts.
6. Time headings and a single filter row in Timeline; per-resource History from search.
