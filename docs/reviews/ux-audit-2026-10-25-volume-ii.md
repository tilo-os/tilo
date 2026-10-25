# UX audit, Volume II — the product, taken personally

Companion to `ux-qol-audit-2026-10-25.md` (Volume I). Base `origin/main` @ `3203631d96`.

## What this volume is, and what it deliberately is not

Volume I catalogued **instances**: 170 numbered defects, each with a `file:line`, grouped by where
the code lives. It is useful for filing work. It is not useful for deciding what Tilo should be.

This volume is the opposite shape. It is written **from the person's side**, ignores implementation
cost, and treats every moment of confusion as a design failure rather than a bug to be patched. It
does three things:

1. **Diagnoses the eight causes** behind Volume I's instances — because fixing 170 instances one at a
   time leaves all eight causes in place, and the 171st instance is already growing.
2. **Walks through realistic people using Tilo** and says, bluntly, where it fails them.
3. **Proposes design moves** — new primitives, new laws, and in several cases deleting surfaces
   outright — rather than hotfixes. Some of these are large. That is fine; several of the defects in
   Volume I cannot be fixed honestly at small size.

**Rule of this volume:** each thing is said **once**. If a problem already has an id in Volume I, it
is referenced, not re-described. If the same design failure shows up in nine apps, it appears once,
as a cause, with the nine named in a list.

**The standard applied.** Not "does it work", but: *does a person know what happened, what will
happen, and what they can do next — without being told?* If a screen requires the manual, the screen
is wrong. If a state requires trusting the author, the state is wrong. If a person has to remember a
chord to reach a capability, that capability is not shipped.

## Contents

| § | Subject | Status |
|---|---|---|
| 1 | The diagnosis: nine causes (P1–P9) | written |
| 2 | Nine people, and where Tilo fails them | written |
| 3 | Thirteen design moves (M1–M13), and what each deletes | written |
| 4 | The tested round — what the pixels and the harness actually said | written; 10 findings, 4 of my own claims retracted |
| 5 | Apps: the promise, the gap, the honest fix | written |
| 5b | Where my own design was wrong — ten corrections to it | written |
| 6 | The bets in order, the habit to stop, and what "finished" means | written |
| 7 | What landed, as commits — and what is still open | written (a snapshot) |

---

# 1. The diagnosis: nine causes

Every defect in Volume I traces to one of eight habits. They are worth naming because they are
*product* habits — they will keep generating new defects until they are decided against.

## P1 — The Polite Lie

**The habit:** when the system doesn't know something, it renders the shape of an answer instead of
the absence of one.

The pattern is everywhere once you look. Four accounts each wearing a "Not checked yet" chip *and* a
sentence saying the same thing, with nothing on the page able to produce a status (`VIS-05`). A
backup destination whose health is the literal `Health::Ok` (`APP-backups-01`). Every live snapshot
labelled "Manual / Daily" regardless of its content. A monitoring card that reports `8.6 GiB` beside
a process row reporting `10.4 GiB`, with nothing saying one is used memory and the other is RSS. A
"Next run: On demand" tile for a scheduler that does not exist (`APP-backups-02`). `1000 rows+` for
a truncated result (`APP-db-client-02`). "All backups healthy" as a constant.

**Why it is worse than showing nothing.** A blank state invites a question. A confident wrong state
closes the question. The person stops looking — which is exactly the moment the truth arrives as a
loss: the backup that was never made, the rule that was never applied, the query result that was
never complete.

**The tell:** any rendered value that is a constant in the code, any "status" that no action can
change, any number whose units or provenance the screen doesn't state.

## P2 — The Vanishing Capability

**The habit:** layout decides what exists, silently.

Narrow a Calculator window and 22 keys cease to exist, with no overflow key, no hint, no way back
(`VIS-01`). Narrow Monitor and the columns leave the card, then the remaining columns overflow it.
Five apps declare no window-bar slots, so their bar is a decorative band with a title and no
capability (`CHROME-04`). Files can search the whole system but not the folder you are standing in
(`APP-file-explorer-01`). The console bell is a documented no-op (`APP-terminal-02`).

**Why it is corrosive.** A person cannot tell "not available here" from "not available at all" from
"the app is broken". Each silent loss teaches them not to look for capability — which is how a rich
system comes to feel thin.

**The tell:** any `if width < …` that changes the *feature set* rather than the *arrangement*.

## P3 — The Raised Hand That Nobody Sees

**The habit:** Tilo refuses to interrupt — correctly — and then gives the person no other channel.

A background app asks for focus, the shell refuses to steal the strip, and the request evaporates
(`SHELL-02`). A build finishes and wants input: nothing. A mention arrives (`APP-chat-05`). An
upload fails (`APP-chat-04`). A long command beeps (`APP-terminal-02`). A tab is waiting for you
three strips away. Meanwhile the bell shows a count with no identity (`SHELL-15`, `SHELL-17`), the
tray shows a dot, the tab shows nothing, and eight private toast types decay at 3, 4, 4.2 and 8
seconds (`DUP-05`).

**Why it is the most expensive cause.** Tilo's whole premise is many windows, many workspaces, no
overlap. In that world, "what wants me right now" is *the* most important question on screen, and
today it is answered by a number with no owner.

## P4 — Undo as a feature, not a law

**The habit:** state is CRDT-native and undoable in the platform, yet each app decides whether to
expose that, and destructive actions get confirmations instead.

Undo-in-toast exists and is used beautifully for *deleting a workspace* — the most destructive act in
the system — while closing a window with five tabs gets silence (`SHELL-05`). Mail's undo dies six
seconds before the send actually happens (`APP-mail-01`). "Discarded empty draft" offers nothing.
Meanwhile a single click in History can revert a shared room with no confirmation (`APP-activity-02`),
and a Kill button is a one-click red primary. So the system is simultaneously careless where it is
irreversible and silent where it is perfectly reversible.

**The inversion to fix:** Tilo has the rare ability to make nearly everything reversible. The right
law is not "add confirmations", it is **confirm only what cannot be undone** — and make undo
impossible to miss everywhere else.

## P5 — Forty Grammars

**The habit:** each app answers the same question differently, so nothing learned in one window
transfers.

Whole-word matching means five different things (`DUP-01`). Ctrl+0 means six (`DUP-02`). Six shortcut
sheets, four of which print chords that are wrong on the person's keyboard (`DUP-03`). Four copies of
Cut/Copy/Paste, one of which shows key hints and one of which offers Cut with nothing selected
(`DUP-06`). Eight toast types with three incompatible severity vocabularies. "This pane is being
read" rendered as skeleton, spinner, frozen sentence, and indeterminate bar (`DUP-08`).

**Why it matters more here than elsewhere.** Tilo is a *stack* where the promise is that collaboration,
undo, search and theming live below apps. A person who feels that promise should feel it in their
fingers: the same chord, the same shape, the same words. Every divergence is the platform leaking.

## P6 — Motion by memory

**The habit:** there is an excellent motion catalogue and no rule about when a thing must move, so
whether a change is explained depends on whether the author remembered.

The autocomplete list pops (`MOTION-02`), the display-change card pops under the pointer
(`MOTION-03`), a modal confirms with 210 ms of arrival and vanishes in one frame (`MOTION-01`), the
calendar swaps an entire month with no follow-through while the desktop performs the same swipe with
motion (`MOTION-06`), chevrons rotate in one widget and snap in three (`MOTION-09`), hover tints
vanish instantly (`MOTION-08`), and the calendar month grid — 30 % of that app — has no hover state
at all (`MOTION-05`).

Motion in Tilo is currently **decoration with good taste**. It should be **explanation**. A person
should be able to close their eyes and still know, from the shape of the change, whether something
arrived, left, moved, was confirmed, or was refused.

## P7 — Chrome by committee

**The habit:** the OS is supposed to own the window, and the window is supposed to be the only place
a window's identity and commands live. In practice, an app may opt out and draw its own.

Three apps draw a full header the bar was supposed to carry (`CHROME-01`, `02`, `03`) — one of them
re-implements the entire bar twice, one draws a 40 px bar under an empty 40 px bar, one silently
opts out because it puts a non-button at the leading edge. Five more declare nothing, so their bar is
a title and a close button (`CHROME-04`). The result a person sees: back is at the top in Files and
40 px lower in Books; search is in the bar in eight apps and absent in five; nothing about "where am
I" is stable.

## P8 — Jobs without bodies

**The habit:** long work has no handle.

An install cannot be watched or cancelled and blocks the whole Store (`APP-store-01`). A backup,
verify or restore cannot be cancelled and blocks the app for its duration (`APP-backups-03`). A
container pull can only be *hidden* (`APP-containers-03`). An offline map download cannot be stopped
(`APP-maps-03`). An attachment upload shows a sentence and cannot be cancelled (`APP-chat-04`). A disk
scan shows a spinner with no progress (`APP-disk-usage-01`). Verification says "Checking every file…"
for minutes (`APP-backups-04`). A hung HTTP request cannot be cancelled and has no timeout
(`APP-api-tester-01`). A long query cannot be cancelled and disables the only button that could
retry (`APP-db-client-01`).

Nine surfaces, one missing concept. The person's mental model is "something is happening and I must
wait"; the system's is "the effect will return when it returns".

**This is the single largest opportunity in the audit**, because the fix is a concept, not nine
patches — and it is a concept Tilo can own outright, since no mainstream desktop has a real one.

## P9 — Capability by coincidence

**The habit:** an app decides what it can do by sniffing its environment — is a binary on `PATH`, is
an environment variable set — instead of being *granted* the capability by the platform.

The Firewall is the clearest case and it is worth stating plainly, because it is not a copy problem:

```rust
// apps/firewall/src/backend.rs:77-84
let has_cmd = which_firewall_cmd();
let armed = std::env::var("TILO_FIREWALL_APPLY").as_deref() == Ok("1");
let mode = if has_cmd && armed { Mode::Live } else { Mode::DryRun };
```

Nothing in `os/` sets that variable, so a packaged Tilo runs `DryRun` — which still plans the
commands, still renders every control as live, and discloses the truth only *after* the person has
acted. The two most consequential controls in the app plan `systemctl stop firewalld` and
`--panic-on`. A person in an incident flips panic mode, watches the switch change, and nothing
happens. **A control that lies in an emergency is worse than one that is missing.**

The variable is not the root cause. The root cause is the mechanism: **an environment variable is a
developer's tool being used as a product behaviour switch.** Feature compilation is the right way to
build a binary without a capability — `#[cfg(feature = "firewall")]`, or simply not shipping the
surface. A safety interlock that the user cannot see is not a safety feature; it is an unmarked
trapdoor, and the safety it provides is the *developer's* (don't touch the host under test), not the
person's.

The same shape appears elsewhere, more quietly: `which podman` deciding whether Containers behaves,
`TILO_PACKAGE_ID` gating a feature in Contacts (`priority_contacts.rs:13`), `TILO_IMAGER_FLATPAK`
changing what imager can do (`helpers.rs:119`), and a fistful of `TILO_TEST_*` overrides. Most of
those are legitimate *test* seams. The defect is that product behaviour and test seams live in the
same namespace, so nobody can tell which is which by looking.

**The law.** *An app may act only because the platform granted it the capability, and a missing
capability is stated once, with a way to grant it — never rendered as an inert control.*

Concretely, for this app: the Firewall gets a **capability** (a mediated connection to the firewall
management daemon, which in Fedora's world is D-Bus + polkit — the request is authorised by the
person, through the same agent the rest of the system already uses), and only this app gets it. Then
it *works*. If the daemon is not running, the app says "the firewall service isn't running — start
it", and shows no switches at all. If a preview is genuinely wanted, it becomes a **user-facing
choice** ("show me what would change" → an apply/discard diff), not a hidden mode where enforcement
silently doesn't happen. That is the whole fix, and it is the same fix for every privileged action in
the system: format a disk, write an image, install software, control containers, open a remote
session, restore a backup.

**The tell:** any `env::var` or `which()` whose result changes what the UI offers a user.

## The nine causes, summarised

| | Cause | The law it should be governed by |
|---|---|---|
| P1 | The Polite Lie | **Nothing is presented as fact without its provenance; "unknown" is a designed state.** |
| P2 | The Vanishing Capability | **Nothing disappears without a door.** |
| P3 | The Raised Hand | **One attention model for the whole system.** |
| P4 | Undo as a feature | **Confirm only what cannot be undone.** |
| P5 | Forty Grammars | **One declaration per command; nothing may drift from it.** |
| P6 | Motion by memory | **Every change of what is shown arrives and leaves.** |
| P7 | Chrome by committee | **The window bar is the only bar.** |
| P8 | Jobs without bodies | **Long work has a body: progress you can read, a way out, and a reason if it fails.** |
| P9 | Capability by coincidence | **An app acts because it was granted a capability — never because its environment happened to allow it.** |

---

# 2. Nine people, and where Tilo fails them

Each story is one ordinary hour. The point is not the individual defects — those are in Volume I —
but the **shape** of the failure: what the person wanted, what they did, and what Tilo made them feel.

---

## 2.1 Kenji — staff engineer, three monitors, hands never leave the keyboard

**Intent:** land a fix. Terminal on the left panel, editor in the middle, logs on the right.

**The hour.** He opens the repo, runs the tests in the terminal, watches them fail in the logs
window, jumps to the editor, fixes it, commits.

- The logs window is set to **Follow** — the one feature he needs — and it does not follow. New lines
  land below the fold; the visible page is frozen. He scrolls with the wheel to the bottom, then the
  next failure scrolls out again (`MOTION-07`). *He stops trusting the window he is watching.*
- The test run finishes. The terminal bell is a no-op. He is in another window, so nothing tells him
  it finished (`APP-terminal-02`, and `P3`). He alt-tabs back and forth for the rest of the day.
- He opens git-client. It draws its own hit rects, so **Tab and Enter do not exist**: he cannot stage
  a hunk, focus the commit field, or reach a branch's Check out without the mouse
  (`APP-git-client-03`). A keyboard-first engineer reaches for the mouse on the fifth action of his
  most repeated task.
- A merge conflict appears. The app badges the file `C` and offers the ordinary Stage/Discard rows.
  There is no "take ours", no "open in Diff & Merge" — the only way out is a terminal
  (`APP-git-client-04`), in an OS that ships a 3-way merge tool.
- History shows no changed files and no per-commit diff, because commit files are never loaded
  (`APP-git-client-02`). He cannot answer "what did that commit touch" in the git client.
- He pushes. Nothing at all confirms it: no toast, no progress, no "3 commits to origin/main". He
  checks the ahead/behind counter to guess whether it worked (`APP-git-client-06`).
- Somewhere in there he presses Ctrl+= in the editor, because text is small on the middle panel. It
  does nothing — the code editor has no zoom at all (`APP-code-editor-01`), though every other
  document app does, and the window-level zoom is shadowed by an app-local handler elsewhere
  (`DUP-02`).

**What it should feel like.** He should never have to *watch* anything. Long work should announce
itself where he is looking (`M3`). Git should be operable from the keyboard because it published its
commands (`M5`). A conflict should lead somewhere. And his chord vocabulary should be the same in the
editor, the diff, the terminal and the browser — because it is one machine.

---

## 2.2 Amara — freelance designer, 13-inch laptop, text at 1.25×, dark room

**Intent:** finish a poster, check it against the client's brief in the chat, export.

- She opens Canvas. It draws its own header row with the artwork title, undo/redo, add picture,
  export and zoom (`CHROME-03`) — 56 px of it — under a 40 px window bar that carries nothing but the
  window controls. On a 13-inch screen she has spent ~100 px of height on two bars that say
  overlapping things (`P7`).
- Undo is in that in-body row. In Files, undo is in the bar. In Sheets, undo is a chord she has to
  remember. Three answers to "undo" in one afternoon (`P5`).
- She wants to show the client a crop of the poster. There is no way to zoom to a region and no
  "capture this window" from the window; she opens Capture Studio, which — under reduced motion,
  which she keeps on because the room is dark and motion nauseates her — ignores her setting and
  fades its receipt anyway (`MOTION-13`).
- She switches to Chat to reread the brief. The image the client sent is a thumbnail she cannot open,
  enlarge, or save (`APP-chat-02`). She screenshots it, which is now a second copy of a file floating
  in her home directory.
- She opens Settings to raise text size a notch. At a narrow window the sidebar keeps its width and
  the content is clipped mid-control (**stale in the current build — see `VIS-07`**). What *is* still
  true: mono internal file paths leak into a settings page as body text in the shipped capture
  (`APP-settings-01`), and "Connected" is colour-only text (`APP-settings-02`).
- Export. Nothing tells her it worked beyond the file appearing somewhere.

**What it should feel like.** One bar, one undo, one zoom that means one thing (`M8`, `M5`). A
setting she changes should change what she sees immediately, everywhere, including inside the app she
is working in. Reduced motion should be a promise the whole system keeps, not a per-widget courtesy
(`M7`).

---

## 2.3 Rosa — retired teacher, 67, touch laptop, large text, dislikes surprises

**Intent:** read her mail, look at photos of the grandchildren, and put a dentist appointment in the
calendar.

- The on-screen keyboard is already up (she tapped a field). Keys are the same size as on any
  desktop and there is no way to make them bigger without changing the whole desktop's density
  (`APP-osk-01`). Her taps land between keys.
- Autocorrect mangles a name. She turns off suggestions to stop it — and loses the suggestion bar
  entirely, because they are the same switch (`APP-osk-02`). Now she types everything by hand.
- She wants to add a calendar event. Every new event starts at 12:00–13:00 and requires a title, two
  date pickers, two time spinners and a time-zone row. She is asked about *time zones*
  (`APP-calendar-01`). She gives up and writes it in the notes field of a mail draft.
- In Mail she selects two messages to file them. A toast appears for four seconds and goes; the
  archive action had no undo (`DUP-05`). She is not sure it happened and does not know how to check.
- The system raised a notification while she was typing. It is gone. The bell has a number on it. She
  taps it and gets a list grouped by app with one group called "System" containing four unrelated
  things she cannot identify (`SHELL-15`, `SHELL-16`).
- Photos: she wants to show a picture across the room and have them advance. There is no slideshow and
  no full screen (`APP-photos-01`).

**What it should feel like.** Reversible things should be reversible and *say so* (`M4`). Anything
that wants her should have a face and a name, and wait for her (`M3`). Settings that matter to her
should be discoverable from the thing they affect — a text-size control where text is, not three
screens away.

---

## 2.4 Priya — screen-reader user

**Intent:** keep up with two project chats, write notes, and review a document.

- In the shortcut guide, ~20 chord rows are real rebind targets for pointer and keyboard, but
  assistive tech receives **one flat dialog node** whose description is a pre-joined string. She can
  hear the shortcuts; she cannot rebind one (`SHELL-10`).
- At the lock screen, the media transport is AT-clickable but keyboard-unreachable (`SHELL-11`), and
  the credential field has no IME path at all (`SHELL-01`) — for her, one of these is an
  inconvenience and the other is an inedible meal: her passphrase is composed through an IME.
- The desktop's clock, calendar, monitor and team widgets have **no accessibility nodes at all**
  (`SHELL-04`). They are simply not there.
- The weather forecast exposes current conditions and air quality, and nothing about the hourly or
  7-day forecasts — which are painted only (`APP-weather-02`). She is told 24 readings exist and can
  read none.
- "Save" in a dialog is greyed out. She cannot reach it (disabled widgets leave the tab order) and
  nothing says why (`CORE-11`).
- An agent run fails with "Managed model planning unavailable: no api key" — lower-case shorthand,
  no provider named, no next step (`APP-agent-02`).

**What it should feel like.** Accessibility in Tilo is architected as *the same door* as automation
— the AGENTS.md says so, and much of the tree delivers on it. The failures above are not the absence
of that architecture; they are places where a surface was hand-built outside it. The fix is
structural, not per-widget: if a surface cannot be reached by the published-action door, it should
not be shippable (`M5`, `M10`).

---

## 2.5 Yusuf, Lina and Bo — three students sharing one workspace

**Intent:** plan a group project across a shared workspace, tonight, in three cities.

- Yusuf drags a file into the chat. "Uploading thesis.pdf…" — no percentage, no cancel. It is 200 MB
  on a bad connection (`APP-chat-04`). He cannot tell whether it is working.
- Lina asks a question in a channel Yusuf is not reading. His system raises a notification he cannot
  act on: clicking it raises Chat but leaves whatever channel was open, so he lands on the wrong
  conversation and hunts (`APP-chat-05`).
- Bo scrolls back three days to find what was agreed. Every message shows a *clock* and no date, and
  the single day divider is scrolled off the top (`APP-chat-01`). He cannot tell today's messages from
  Tuesday's.
- Yusuf edits a shared note and closes the window. Nothing anywhere said the note had unsaved work
  (`DUP-04`), so he does not know whether the others see his change. In fact autosave handles it — but
  the system never said so, and one of the apps in the tree does have this indicator, so his mental
  model of "does Tilo save?" is now app-dependent.
- Lina tries to undo a paste that went wrong in a shared doc. Undo exists. What it undoes, and whose
  changes it touches, is not something the UI ever explains (`M4`).

**What it should feel like.** In a collaborative OS, the two most important sentences are "did that
reach the others?" and "what changed while I was away?". Tilo has the CRDT to answer both instantly
and shows neither. This is the single clearest place where the product's architecture is far ahead of
its interface.

---

## 2.6 Tomás — sysadmin, on call at 03:10

**Intent:** a box is misbehaving. Restart a container, check logs, look at resource use, and stop
being paged.

- He opens the container's **Logs** tab. It is a frozen snapshot from when he selected it: logs are
  fetched once and carried over on every re-probe (`APP-containers-01`). The tab reads as live. He is
  reading history while believing it is now.
- He clicks Stop. Nothing happens for seconds — no in-flight state, buttons stay enabled — so he clicks
  again. The confirmation toast says "stop accepted for 9f3c1a7be4d2", naming the container by raw id
  with an uninflected verb (`APP-containers-02`).
- Monitor: he wants to know *when* the spike happened. The sparkline cards have no time axis, no range,
  no hover readout (`APP-monitor-01`); the Memory card and a process row show numbers that look like
  they contradict each other, with no statement of which measure each is (`P1`).
- He checks the firewall, because he suspects a dropped connection. There is no view of blocked
  traffic at all (`APP-firewall-03`) — the primary diagnostic loop of a firewall is missing. He then
  hits **panic mode** to sever everything, watches the switch flip, and nothing changes: the build
  plans `--panic-on` and never runs it (`APP-firewall-01`). He believes he has just dropped all
  traffic. He has not.
- He opens a remote SSH session to the host. It hangs on "Connecting" with no cancel and no timeout
  (`APP-remote-01`). There is no file transfer, so he `scp`s from a terminal he opens beside it
  (`APP-remote-03`).

**What it should feel like.** At 03:10 a person needs three things: *now*, *why*, and *undo*. Every
one of Tomás's failures is one of those three being unavailable while the screen looks confident.
The firewall one deserves a sentence of its own: **a control that lies in an emergency is worse than
a control that is missing.** Deleting panic mode is a better product than shipping it inert.

---

## 2.7 Ines — teacher, projector, forty people

**Intent:** show a deck, then photos, then get back to her notes.

- Slides cannot be zoomed; the canvas is always fit-to-window (`APP-slides-01`). She cannot nudge a
  text box 1 px or read 9 pt body copy while editing — and in front of the class she cannot zoom into a
  diagram at all.
- She cannot copy a shape between slides, and cannot paste a screenshot into a deck
  (`APP-slides-03`). She rebuilds the shape by hand, twice.
- During the presentation she wants to zoom into a chart. There is no zoom in the presentation.
- Photos has no slideshow and no full screen (`APP-photos-01`), so the window chrome, sidebar and
  filmstrip stay on the projector all lesson.
- Afterwards she prints the handout. Print Centre offers "Send to", "Copies", "Print on both sides"
  (`APP-print-centre-01`). Not a page range. She prints 40 pages to hand out pages 3–5.

**What it should feel like.** Presenting is a *mode*, not a window size. It should be one keystroke
from any content app, it should hide everything that isn't the content, and it should be obvious how
to leave. (See `M12`.)

---

## 2.8 Sam — first hour with Tilo, no manual

**Intent:** work out what this thing is.

- He opens an app with nothing in it. Some apps show a real, focused empty state with a call to
  action; some show a rectangle of background (`VIS-11` lead), some show "Editor coming soon".
  He cannot tell "empty" from "not loaded" from "not finished".
- He learns that windows never overlap and live in strips. Nothing on screen explains *strips* — the
  window menu lists every strip and workspace as places, which is the discoverable version, but it
  only appears on right-click (`P7`). The cap on how much he can see is not a wall: it is a door, and
  it is unlabelled.
- He presses Super+PageDown on the last workspace. Nothing happens — no motion, no indicator, no
  bounce (`SHELL-07`). He does not know if he is at the end, or if the chord failed, or if an app ate
  the key.
- He wants a window half the screen. He drags a seam to 43/57 and there is no "equalise", no "make
  this half" (`SHELL-06`).
- He right-clicks a tab: a real menu with shortcuts beside each row. Good. He right-clicks a cell in
  Sheets: nothing at all (`APP-sheets-01`). He concludes right-click is unreliable and stops trying.
- He closes a window with five tabs. No message. He assumes it is gone; it is in fact reopenable, but
  he will not learn that for weeks (`SHELL-05`).

**What it should feel like.** The first hour should be *one* discovery, not forty. Tilo's best
surfaces (the window menu, quick settings, the palette, the onboarding) are genuinely excellent. The
problem is that the rest of the system does not always keep the promise those surfaces make, so
whatever he learns first, he will have to unlearn somewhere.

---

## 2.9 The shared household machine

**Intent:** a parent's laptop, also used by a teenager, also visible to whoever walks past.

- TOTP codes are painted on every row, always, with no lock or reveal gate (`APP-totp-01`). Second
  factors are readable from across the room and in any screen share.
- A copied snippet or password: Passwords expires its clipboard; Snippets uses a non-expiring,
  unchecked copy path (`APP-snippets-03`).
- Notification banners carry no app identity (`SHELL-15`), so the family cannot even tell at a glance
  whether a banner is work, school or something private — and content is on the banner by default.
- A notification from anything not launched by the shell is filed under "System" with no name
  (`SHELL-16`).

**What it should feel like.** Privacy is a *display* property, and Tilo does not currently have a
concept of "this content is for its owner's eyes". Every app answers it separately, and most answer
"no".

---

## 2.10 — The tenth story: an hour that worked, and what must not break

Nine stories of failure teach what to fix. This one exists so the redesign knows what it is not
allowed to break, and so a reader can see that the diagnosis is not contempt.

**Rosa again** (from §2.3), same machine, a different afternoon.

She photographs the last of the tomato harvest and plugs the camera in. Imager opens on the
import: it says how many pictures, how much space, and where they will go — and when she changes
her mind and presses Import again with a different folder, it asks once, plainly, before
overwriting anything. **The destructive path in Imager is the best-designed flow in the entire
audit**: it names what will be lost, in her words, before it is lost. Whatever else changes, this
must not be simplified into a single "Continue".

She drags the folder into Documents. Files notices two files with the same name and asks her to
choose per file, showing sizes and dates side by side, and the one she keeps is the one she kept —
then her mistake is recoverable from Trash, which says what it holds and how much space it takes.
**Files' conflict and trash handling is the reference implementation for the whole system.**

She opens a letter she has been writing and the editor says, in its bar, that it is unsaved, and
when the window closes by accident the app offers to keep her work rather than asking a question
she cannot answer. **The code editor's unsaved handling is what every editor here should copy.**

Then she narrows the window to read a long line, and the settings pane folds into one column that
still shows everything, with a back button where a back button belongs. That is the one narrow
mode in the product that is already right (§4.2 `T1` is that same code, failing only when the
window *becomes* narrow rather than opening that way).

Finally she picks a high-contrast theme. The theme refuses to apply if it cannot keep text
legible, and says so. **That refusal is the single best product decision in the repository**: it
is the system declining to let the person make an unreadable desktop.

Four things to protect, then, stated as the regression charter this audit will be measured against:

1. **A destructive act names what it will destroy, in the person's terms, before doing it.**
   (Imager.)
2. **A conflict is a question about specific items, never a bulk decision.** (Files.)
3. **Unsaved work is a state the interface shows, not a question it asks at the worst moment.**
   (Code editor.)
4. **The system may refuse to make the interface illegible.** (Themes.)

Any redesign in §5 that weakens one of these is wrong, no matter how much tidier it looks.

# 3. Thirteen design moves, and what each one deletes

Thirteen moves. These are not fixes. Each is a *concept* that replaces a class of defects, and each is judged on
whether a person would feel it without being told. For every move: the law, the mechanism, what it
absorbs, what it **deletes** (deletion is the cheapest UX win available), and what it costs.

---

## M1 — The Provenance Rule: nothing is a fact without its as-of

**The law.** Every value the system presents as true carries *when it was true* and *where it came
from*. "Unknown" is a designed state with a reason and a way to resolve it. Zero is never a stand-in
for unknown.

**The mechanism.** A first-class `Fact` type that the toolkit renders — not a string the app builds:

```
Memory used    8.6 GiB
               measured 2 s ago · this device        ← caption, always
─────────────
Dentist, Tue 14:00
               from Calendar · updated 3 d ago       ← ages visibly
─────────────
Sync status    —   not checked
               no keyring available on this device   ← why, plus an action
```

Three typographic rules make it teachable: provenance is always the caption line under the value,
never beside it; a fact that has not refreshed within its own expected interval gains an "ago" and
its provenance dims (the *value* never dims — dimming the number is how you make people distrust
good data); and an unavailable fact renders an em dash plus a reason, never `0`, never `—` alone.

**Absorbs.** All of P1: `VIS-05`, `APP-backups-01`/`-02`/`-07`, `APP-db-client-02`,
`APP-monitor-01`, `APP-weather-03`, `APP-settings-02` (status is a word, not a colour),
`APP-containers-01` (a frozen log is a fact with a very old as-of).

**Deletes.** `Health::Ok` as a constant. "All backups healthy" as a toast. Four repetitions of "No
synchronization status is available yet." The bare `+` on a truncated result. The distinction between
"0 %" and "unavailable" that Containers had to write a regression test for.

**Why it beats the platforms.** Every OS shows stale data as current; the tell is the phantom
"last synced 2 minutes ago" that has said that for an hour. Tilo is replicated — "as of" is *already*
in the data model and is cheap to surface. Making provenance a visible, consistent typographic
convention is something no desktop has done, and it directly serves the thing Tilo claims to be:
a system you can trust with shared state.

**Cost.** One widget, one theme role, and one policy check that fails a build when a status position
interpolates a raw number or when a `Fact` is rendered without provenance.

---

## M2 — The Job: long work gets a body

**The law.** Anything that can take longer than a breath is a *job*: it has a name, a subject, a
progress you can read, a way out, and an outcome — and it survives the window that started it.

**The mechanism.** A system primitive with five fields that every long effect returns:

| field | meaning | example |
|---|---|---|
| title | what is happening, in the person's words | "Installing LibreOffice" |
| subject | what it is about, as a link | `store/libreoffice@7.6` |
| progress | determinate when known, honest when not | "312 MB of 480 MB · 1 m 20 s left" |
| cancel | present unless cancelling would be destructive | a button, not a hope |
| outcome | done / failed **with a reason** / cancelled | "Failed: 480 MB needed, 210 MB free" |

**The surface.** One place — call it **Jobs** — reachable from the bar's activity pill (which appears
only while something runs), from the palette, and from wherever the job was started. One row per job:
live line, cancel, and "show me" which takes you to the thing being worked on. Jobs are *owned by the
system, not the app*: start an install in Store, close Store, the job keeps running and stays
watchable. Failed jobs do not disappear — they become attention items (`M3`) with the reason attached.

**Absorbs.** All of P8 and its nine surfaces: `APP-store-01`, `APP-backups-03`/`-04`,
`APP-containers-03`, `APP-maps-03`, `APP-chat-04`, `APP-disk-usage-01`, `APP-api-tester-01`,
`APP-db-client-01`, plus print queues, video export, thumbnail batches and PDF imports.

**Deletes.** "A package operation is already in progress" as a *global* refusal — jobs are concurrent
by construction, so the Store stops being unusable while installing. Nine different progress idioms.
The modal that can only be hidden, not cancelled. The "wait for it to finish" sentence.

**Why it beats the platforms.** Check any OS: browsers have their own download shelf, the store has
its own queue, backups have their own window, and closing the app loses the thread. Nobody offers
"everything long-running, in one list, cancellable, with reasons". Tilo's version is *worse today*
than macOS precisely because it scatters the same concept across nine apps with nine behaviours.

**Why it is cheap here.** The prototype already exists three times: browser downloads have progress,
cancel and retry; video export has progress and a working cancel; runtime repair streams byte counts.
The work is not inventing the pattern, it is promoting it to a system concept and making effects
declare it.

**Cost.** Effects must become cancellable and progress-reporting. That is real work — and it is the
single highest-value work in this list, because it converts "the app is frozen" into "the app is
doing something and you can stop it" across the whole product.

---

## M3 — The Attention Ledger: one raised hand, with a face

**The law.** Nothing may ask for a person's attention except through one model, and every item in it
must say *who* is asking, *what* about, and *what you can do* — and it must wait.

**The mechanism.** An `Attention` record — source, kind, subject, since, actions, state — produced by
everything that currently improvises:

- a focus request the shell refuses (`SHELL-02`)
- a mention (`APP-chat-05`)
- an alarm due, a timer finished
- a job that failed or needs a decision (`M2`)
- a close blocked by unsaved work
- a terminal bell (`APP-terminal-02`)
- a remote session that ended (`APP-remote-01`)
- a moderation flag, a backup that never ran

**The surface.** **Needs you** — a stack, ordered by urgency then age, each row carrying the app's
verified identity (icon *and* name, never icon alone) and one primary action. Reachable by one chord
from anywhere; `Up/Down` moves, `Enter` acts, `Escape` defers. **Deferring is a first-class verb and
is visible**: "4 deferred" stays on screen, because a quiet system must still be honest about what it
is holding back.

Two states, deliberately distinct: **unseen** (the count on the bar) and **seen but still open** (the
row stays, but stops counting). Dismissing is not seeing; a person who swipes something away has not
read it, and the system should not pretend otherwise.

**Absorbs.** `SHELL-15`, `SHELL-16`, `SHELL-17`, `SHELL-02`, `APP-chat-05`, `APP-terminal-02`,
`APP-mail-02`, `APP-alerts`-style failures, and every "something needs you" toast.

**Deletes.** The bell as a number with no owner. Per-app badge counts that are collected, filtered and
never drawn. The tray status dot. Eight private notice types whose only job was to say something
happened. The "System" group nobody can identify.

**Why it beats the platforms.** macOS Notification Centre is a feed of promotions and interruptions
with no notion of "waiting"; Windows is a toast slot machine; GNOME drops notifications in a list you
never open. Tilo has an unfair advantage: it *is* the window manager, so it can say "Strip 2 · Files
wants input" and take you there in one action. That is a categorically better answer than "Files: 3".

**The hardest design question, answered.** How do you avoid becoming noise? Three rules: an item must
have an *action*, an item must have an *owner*, and an item must be able to *expire into history*
after being seen — after which it lives in the ledger (`M9`), not on screen. Anything that cannot
satisfy all three is not attention, it is a log line.

---

## M4 — Undo as the spine: time travel, and confirm only the irreversible

**The law.** Tilo can make almost everything reversible. So: **confirm only what cannot be undone**,
and make undo impossible to miss everywhere else. A confirmation is an admission that the system
failed to provide a way back.

**The mechanism, part 1 — the scrub.** Undo is not a stack of clicks; it is a *seam you can pull*.
Press and hold the bar's back action (or hold `Ctrl+Z`) and a scrub appears: the surface re-renders at
each step as you drag, with a one-line summary of what you are about to undo — "undo 3 changes: title,
2 blocks" — and the people whose changes are involved. Release to commit, `Escape` to abandon.
Interruptible, previewable, and impossible to overshoot.

**The mechanism, part 2 — scopes, stated in words.** Two scopes exist and the UI says which it is
using: **Mine** (my changes in this surface) and **Everyone** (the room's history). Plural steps carry
the other people's avatars, because in a collaborative system "undo" is a social act and pretending
otherwise is the bug.

**The mechanism, part 3 — the irreversible list.** Everything not on this list has undo:

> send mail · apply firewall rules · format/erase a device · kill a process · sign/publish ·
> grant a permission · pay

For those, one confirmation *style*, used everywhere: the **consequence sentence** as the body
("This will drop all network traffic on this device"), the object named ("build-box"), and the action
labelled with the verb, not "OK". No exclamation marks, no red for the sake of red — red only where
the loss is permanent, matching `M7`'s Refuse motion.

**Absorbs.** `SHELL-05` (closing a window gets an undo toast — the capture already exists),
`APP-mail-01` (the undo window must equal the hold window; today the undo dies 6 s early),
`APP-activity-02` (restore becomes reversible instead of confirmed),
`DUP-04` (three divergent "Save changes?" prompts collapse: we already saved, here is the moment
before), Calendar's "Discarded empty draft", and `APP-monitor-03`'s Kill (which *is* irreversible, so
it keeps a confirmation — and gains Term/Interrupt beside it).

**Deletes.** Most modals. The "are you sure?" that exists because nobody wired undo. The 8-second
undo of a *workspace deletion* being the best undo in the system while closing five tabs has none.

**Why this is Tilo's best idea.** Every mainstream desktop has a linear, per-app undo stack, so nobody
can offer "scrub the last minute of this document, including what your collaborators did". Tilo's
state is CRDT-native and attributed. Executing this well would be the single most distinctive
interaction in the product — the thing people show each other.

**Cost.** The data is there. The work is the scrub UI, the scope semantics per surface, and the
discipline of classifying every action as reversible or not (which is also what `M5` needs).

---

## M5 — The Chord Law: one declaration, and nothing may drift from it

**The law.** Every action a person can invoke is declared **once**, and everything that exposes it —
bar slot, menu row, context menu, palette entry, shortcut sheet, accessibility node, agent action,
tour step — is *generated* from that declaration. An app may not hand-draw a command surface.

**The mechanism.** A command declaration:

```
command: sheets.cell.delete
label:   Delete row
scope:   sheets.grid · selection = rows
chord:   primary+Shift+K          (declared deviation: Delete keys are row content)
enabled: selection.rows > 0
does:    removes the selected rows
reversible: yes — undo restores rows and their cells
attention: none
```

From that one record the system derives: the menu row *with its chord rendered for the current
keyboard*, the palette entry and its ranking, the a11y node with role and name, the shortcut sheet
row, the agent action, and the "is this enabled, and if not why" answer (`CORE-11`).

**The chord grammar.** Chords get named slots so that learning one window teaches all of them:
`primary+F` find · `primary+S` save/export · `primary+Z` undo · `primary+O` open · `primary+W` close ·
`primary+0/=/-` zoom reset/in/out · `F2` rename · `Shift+F10` context menu · `?` this window's
commands. A deviation must be *declared with a reason*, exactly as the codebase already makes callers
declare deviations from its formatting and clippy rules.

**Absorbs.** `DUP-01` (one find semantics, declared), `DUP-03` (six sheets become one generated
sheet, with correct chords on the person's keyboard), `DUP-06` (one edit-menu vocabulary, generated),
`SHELL-10` (rebind rows get their a11y nodes for free), `CORE-05`/`CORE-06` (menu/dropdown keyboard
vocabulary), `CHROME-04` (a bar with no slots becomes impossible — the commands exist, so the bar
shows them), `APP-git-client-03` (painted targets become declared targets), `APP-git-client-08` and
the other untranslated labels (a label is a key, not a string).

**Deletes.** Six shortcut sheets. Four edit menus. Two of six sets of chords that are wrong on macOS.
The `Vec<Option<Action>>` dispatch pattern. The "why is this greyed out?" mystery.

**Why it beats the platforms.** macOS has per-app menus that drift from their shortcuts; GNOME's
headerbar apps each invent a keyboard model. No OS can promise "learn one window, know them all" —
because none has a registry. Tilo can, and can *enforce* it with a check that fails a build when a
painted control is not backed by a declared command. That check is the natural successor to the
existing hand-rolled-UI audit.

---

## M6 — Nothing disappears without a door

**The law.** Width changes **arrangement**, never **capability**. When something cannot fit, it folds
into a door that *names what it holds*.

**The mechanism.** An `Overflowable` container: children declare a fold priority; when space runs
out, folded children collapse into one labelled control whose label is content, not an ellipsis —
"More keys: tan, π, e, % — 12 more". Choosing the door opens the full set in a popover that is the
same widget, so nothing is *missing*, only *folded*.

Rules:
1. Folding is announced once, the first time it happens in a surface ("Your window is narrow — 12
   function keys moved here").
2. A folded set always preserves the *primary* row (digits, the selected row's actions) — the
   essential stays, the optional folds.
3. Nothing may remove a capability by width alone. If a layout cannot fit, it scrolls, wraps, or
   folds; it does not silently narrow its own feature set.

**Absorbs.** `VIS-01` (Calculator's 22 vanished keys — the door is the honest narrow layout),
`APP-monitor`'s columns leaving the card, `APP-osk-01` (key size is a *preference*, not a
consequence), `CORE-08`/`CORE-09` (target minimums, including the 48 px coarse floor),
`APP-firewall-02` (a numeric keyboard for a CIDR field is a capability failure: the door here is the
*right* keyboard, per field kind), `CHROME-04`.

**Positive reference already in the tree.** Settings at 438 px is exactly the right behaviour —
narrowing changes the *navigation model* (list screen → content screen with a back action), not the
feature set (`VIS-07`). That is the model to generalise.

**Why it beats the platforms.** Every desktop silently hides toolbar items in overflow menus labelled
`»` or `⋯` with no indication of what is inside. Named doors are strictly better and cost nothing but
discipline — and in a system whose whole premise is that capabilities are addressable, a silent
capability loss is a contradiction.

---

## M7 — Motion as explanation: eight intents, and no widget may invent one

**The law.** Every change to what is shown is one of eight intents, and each intent has exactly one
motion, system-wide. A widget does not choose an animation; it declares an intent.

| Intent | When | Motion | Reduced motion |
|---|---|---|---|
| **Arrive** | something appears that was not there | scale from its origin + fade, `base` | fade only |
| **Leave** | something stops being shown | fade + settle back to origin, `fast` | fade only |
| **Relocate** | something is still there, in a new place | travel on `Snap`, following the finger if a finger is dragging | land |
| **Confirm** | an action succeeded and the surface says so | a mark that draws itself once, `fast` | land |
| **Refuse** | an action was rejected | a short lateral shake, plus the reason in words | land |
| **Attend** | something wants the person (`M3`) | a single pulse on the owner's own surface, once | land |
| **Busy** | work is running (`M2`) | a slow loop, never a fast flicker | hold |
| **Settle** | a value in a control changed | interpolate that control's own channel | land |

Three rules make it enforceable, and each one kills a specific class from Volume I:

1. **Nothing appears at zero → one without arriving** (kills the popping autocomplete list `MOTION-02`
   and the display-change card `MOTION-03`).
2. **Nothing leaves in one frame** (kills the modal that confirms with 210 ms of arrival and vanishes
   instantly — `MOTION-01`).
3. **A declaration that changes state interpolates its own channel, always** — thumbs, marks, chevrons,
   progress fills, hover tints (`MOTION-08`, `-09`, `-10`, `-11`). These are not "animations"; they are
   the difference between a control that feels connected to your hand and one that teleports.

**Reduced motion is a promise, not a courtesy:** "fade, never travel, never cut". Today a capture
receipt ignores the setting entirely (`MOTION-13`) and three widgets turn a specified fade into a cut
(`MOTION-14`). Under the law those are bugs of the same severity as a wrong label.

**Why this beats the platforms.** Every desktop ships an animation *library* and no *rule*, so the
same act animates in one place and snaps in another and nobody can say which is correct. An intent
vocabulary is testable: for each intent, render the before and after; if no transition exists, fail.
It also gives reviewers a shared word — "that should be Relocate, not Arrive" — which is how a system
stays consistent after the audit is forgotten.

## M8 — One chrome: the window bar is the only bar

**The law.** A window's identity and its commands live in the window bar. An app declares *slots*; it
never paints a top row of its own. Full-bleed surfaces declare a header and the bar merges into it —
but they still declare, so search, history, undo, jobs and attention are never lost.

**Why it is not merely tidiness.** The bar is where three of this volume's system surfaces land
(`M3` attention, `M2` jobs, `M4` undo history) and where `M5`'s generated commands appear. An app that
draws its own row is an app that opts out of all four — which is exactly what Books, Typst and Canvas
do today (`CHROME-01`–`03`), and why Back sits 40 px lower in Books than in Files.

**The enforcement, which is the actual design work.** A policy check that fails a build when an app
paints interactive ink above its content region without a slot declaration. The repo already runs a
family of such checks (`tools/policy/`), so this is a familiar shape rather than a new idea — and it is
the only way "the OS owns window chrome" stops being a sentence in `AGENTS.md` and becomes true.

**What it deletes:** three full re-implementations, five bars carrying nothing but a title
(`CHROME-04`), two bar heights (40 px with 4 px insets vs 56 px with 12 px — `CHROME-02`), two heading
roles for the same job, and the mail composer's hand-drawn panel title bar (`CHROME-07`).

## M9 — Honest ephemera: toasts are deleted as a category

**The law.** Nothing the system says about itself is on a timer. An outcome goes to exactly one of two
places: **Needs you** (`M3`) if it requires a person, or **the ledger** — an append-only record of what
happened — if it does not.

**Why.** A toast is a claim that something matters *right now* and will not matter in four seconds.
Both halves are usually false, and the timer makes the system untrustworthy in a specific way: the
person learns that messages disappear, so they stop reading them, so the ones that mattered are missed
too. Volume I counts eight private notice types, three incompatible severity vocabularies, four
different lifetimes (3 s, 4 s, 4.2 s, 8 s), and one app that decides severity by testing whether the
message text starts with "Couldn't" (`DUP-05`).

**The mechanism.** The ledger is not new work: it is `apps/activity`'s history, which already has
attribution, filters and revert. What is new is that *the system writes to it by default* and shows it
in one place — a scrollable line of what just happened, reachable from the bar, never stealing focus,
never expiring. "Backup finished", "3 files moved to Trash", "Pushed to origin/main", "Rule applied" all
live there. The person can ignore it forever, and it is still there when they need it.

**The one exception, stated so it cannot creep back:** if a machine needs a decision from a human
("this device may be about to lose your network"), that is not ephemera — it is `M3`, and it waits.

## M10 — Design review as a gate: the thing that stops all of this recurring

**The idea.** The 170 findings in Volume I are not 170 mistakes; they are ~9 causes reproduced across
surfaces. The only way that stays fixed after everyone forgets this audit is to make the *evidence*
automatic. The harness already exists — `--shots-dir`, `--responsive-matrix`, `--viewport`,
`--text-scale`, `--pseudo-locale`, `--window-bar`, `--theme` — and the repo already treats captures as
PR evidence. It is one step from evidence to gate.

**The command.** `tilo-design-review [--app X]`, which renders the matrix and asserts:

| Assertion | Catches |
|---|---|
| no ink outside a clipped parent | every clipped label, every overflow at narrow width (`VIS-01`, `T1`) |
| every painted label fits its rect | the calculator button that clips its own translation (`T4`) |
| text contrast ≥ floor for its role | colour-only status, dim captions (`APP-settings-02`) |
| no pseudo-locale delimiter appears twice | the double-localisation defect (`T3`) |
| every list has a staged empty state, every fetch a loading state | the four presentations of "reading now" (`DUP-08`) |
| each intent pair differs by a transition | any change that pops or cuts (`M7`) |
| `construct-at-W` == `resize-to-W` | the resize bug that three passes missed (`T1`) |
| the font catalog is the production one | reviewing fallback typography (`T2`) |

The last two are the ones I would never have written without this round, and they are worth more than
the other six together: one found a bug that only exists on resize, the other found that *all* the
review evidence was measured in the wrong typeface.

**What it refuses to do.** It does not judge taste. It cannot tell you a screen is dull or that a
metaphor is wrong — that stays human, and should. It answers only "is this surface able to be looked at
without embarrassment", which is the floor, not the goal.

**Why it is the highest-leverage item in either volume.** Every other move here is a thing to build.
This one is the machine that keeps them built. Without it, this audit is a snapshot of a codebase that
will drift back within two quarters.

## M11 — Presence, made ordinary

**The problem.** Tilo's collaboration is below the apps — which is right — but the *interface* of
collaboration is nowhere. A person cannot answer the two questions that matter in shared work: **who
else is here**, and **what changed while I was away**. Today: `Activity` has a lens for it, presence
colour exists in the theme, avatars appear in lists, and none of it composes into an answer.

**The mechanism.** A **Presence line** in the window bar (not a panel): the people in this surface,
their colour, and a count of changes since you last looked. Press it → the timeline for *this*
surface: "Lina edited 3 blocks · Bo commented · you changed the title". This is `M4`'s history view
narrowed by surface, so it costs one query, and it makes the CRDT visible for the first time.

**Why it beats the platforms.** Google Docs shows cursors and a version history buried three menus
deep. Notion shows avatars and "edited by". Nothing mainstream puts *what changed since you last
looked* one press from the thing you are looking at — which is the actual anxiety of shared work.

**The rule that keeps it honest:** presence is about *this surface*, never a global "who is online"
list. A global list is a distraction; a surface-local answer is an answer.

## M12 — Present: a mode, not a window size

**The law.** Any surface that shows a thing to a room can enter **Present** in one action from the bar
or one chord. Present means: the content fills every display it is given, chrome withdraws to a
one-line strip that appears on pointer movement, advance and back are single keys, and `Esc` leaves —
always, from anywhere, with no confirmation, restoring exactly the layout you left.

**Why a mode and not "fullscreen".** Fullscreen is a *window* property, so every app re-implements it
and each gets it slightly wrong; Slides, Photos, PDF, Video, Books and Maps all currently approximate
it or skip it (`APP-photos-01`, `APP-slides-01`). A mode owned by the system can also do the things a
window property cannot: pick the projector display, show presenter notes on the laptop while the deck
is on the wall, keep the clock out of the way, and refuse to show notifications (which is a *promise*
the person can rely on in front of forty people).

**What it deletes:** five per-app fullscreen approximations, and the "did my window hide the dock or
did the app?" ambiguity.

## M13 — The capability door: privilege granted, never inferred

**The law.** An app may perform a privileged act only because the platform granted it that capability,
for that app, to that object. A missing capability is stated once with a way to grant it — never
rendered as an inert control, and never discovered halfway through an action.

**The mechanism.** Each privileged verb declares `requires: Capability` — `network.firewall`,
`storage.format`, `media.write`, `packages.install`, `session.remote`, `backup.restore`. The platform
resolves it (daemon reachable, policy allows, person authorises through the existing agent), and the
result is one of three, all of which the UI must be able to render:

| Resolution | The surface shows |
|---|---|
| **granted** | the real control, doing the real thing |
| **grantable** | the control *plus* an explicit grant step, named in the person's words ("Tilo needs your permission to change firewall rules") |
| **unavailable** | no control at all, one sentence of why, and what would make it available ("the firewall service isn't running — start it") |

Three consequences, and each one is a defect class from Volume I:

1. **No inert control may exist.** The Firewall's switches and Apply either work or are not drawn
   (`APP-firewall-01`). The general rule, applied to every privileged app in the tree.
2. **No environment sniffing in the product path.** `which()` and `env::var()` are test seams, kept in a
   separate namespace and never consulted for what the UI offers (`P9`).
3. **Grant is an object-scoped action, not a global switch.** "Allow Containers to manage its own
   containers on this device" is a different grant from "allow it to format a disk", and a person who
   revokes one should not lose the other.

**Why this is the right shape for Tilo specifically.** The project's stated security boundary is the
sandbox and the portal — capability, not trust. This makes the *UI* obey the same model the *system*
already does, and it turns the sandbox from a thing that blocks features into a thing that explains
itself. It also makes Firewall, Imager, Containers, Remote and Backups stop each inventing their own
version of "can I do this?".

## The thirteen moves, and what each one removes

| Move | Removes | Adds |
|---|---|---|
| M1 Provenance | faked status, unlabelled measures | a `Fact` with an as-of and a source |
| M2 Jobs | nine ad-hoc progress stories | one cancellable job with a body |
| M3 Attention | bell counts, badge dots, 8 toast types | one waiting stack with owners |
| M4 Undo spine | most confirmation dialogs | time travel + an irreversible list |
| M5 Chord law | 6 sheets, 4 edit menus, drifting chords | one command declaration |
| M6 Doors | silent capability loss by width | named overflow |
| M7 Motion law | pops, cuts, per-widget animation | 8 intents, enforced |
| M8 One chrome | 3 re-implemented bars, 5 empty bars | the bar as the OS's surface |
| M9 Ledger | every toast | a record that does not expire |
| M10 Design gate | repeating the same 9 causes | automatic evidence |
| M11 Presence | "who's here?" anxiety | one press to what changed |
| M12 Present | 5 fullscreen approximations | a mode with a promise |
| M13 Capability door | `which()`/env-var privilege, inert switches | granted, grantable, unavailable |

---

# 4. The tested round: what the pixels and the harness actually said

Everything below was executed against this commit, with the app's own offscreen renderer. Commands
are given so each result is reproducible. The purpose was not to confirm Volume I but to *try to
break* the surfaces a person actually meets — and, importantly, **to test the test**.

## 4.1 What was run

```sh
cargo build -p calculator -p settings        # via the repo's remote runner, ~4–5 min each
./target/debug/calculator --shots-dir /tmp/uxr/calc --theme dark          # 11 frames
./target/debug/calculator --shots /tmp/uxr/calc-hc.png --theme high-contrast
./target/debug/calculator --shots /tmp/uxr/calc-scale.png --text-scale 1.5
./target/debug/calculator --shots /tmp/uxr/calc-pseudo.png --pseudo-locale
./target/debug/calculator --first-frame
./target/debug/settings --shots-dir /tmp/uxr/settings --theme dark       # 74 frames
./target/debug/settings --responsive-matrix /tmp/uxr/settings-rm         # 4 presets
for w in 380 400 420 440; do ./target/debug/settings --shots /tmp/uxr/set-$w.png --viewport ${w}x800; done
./target/debug/settings --shots /tmp/uxr/set-390x844.png --viewport 390x844
./target/debug/settings --shots /tmp/uxr/set-390-scale15.png --viewport 390x844 --text-scale 1.5
./target/debug/settings --shots /tmp/uxr/set-390-pseudo.png  --viewport 390x844 --pseudo-locale
```

Contrast was measured from the PNGs numerically (WCAG relative luminance), not judged by eye.

## 4.2 T1 — notable — Narrowing a window does not re-derive Settings' layout; it clips. The bug only exists on *resize*.

Three tests located this, and the reason it survived every earlier reading is that it **does not happen
when a window opens narrow** — only when a window *becomes* narrow.

**Test 1 — the matrix.** `--responsive-matrix` produced `so.tilo.settings-phone.png` at 390×844 showing
the desktop two-column layout clipped on both edges: nav labels cut to their last syllables ("…ce",
"…ity", "…ons"), content cut mid-word ("Appearanc", "Theme ar", "Densit", "Accessib").

**Test 2 — the same width, a fresh window.** `--viewport 390x844` renders the correct narrow pane. So
do 380, 400, 420, 440 and the declared `26-narrow` scenario at 438. The app *can* do it.

**Test 3 — what is different between the two?** Read the harness: the matrix **constructs the app at
its declared shot viewport (1280×800 for Settings) and then calls
`driver.resize_component_capture(viewport)`** (`launch.rs:962-966`). `--viewport` constructs it at 390.
So the matrix frame is the *resize* case, and it is the only one that breaks.

Compare another app through the identical path:

| App | matrix phone frame vs `--viewport 390x844` |
|---|---|
| Settings | **different** (`2597cad9…` vs `d71362aa…`) — clipped wide layout |
| Monitor | **byte-identical** (`898b5bf0…` == `898b5bf0…`) |

The harness relayouts — Monitor proves it. Therefore **Settings caches the narrow decision instead of
deriving it from its current rect.** The evidence is in the app: `settings_app.rs:57-70` keeps
`narrow_screen: NavPane` and a "sidebar starts narrowed to it" staged value, and its own doc says the
shell owns the breakpoint. So the decision is made once, at construction, from what the host said then
— and a later resize changes the rect without changing the decision.

**Why this is a user-facing bug, not a capture quirk.** Every person who drags a Settings window
narrower — or whose tile is resized by the shell when a window opens beside it, which in a tiling
desktop is *constant* — gets a desktop layout clipped into a narrow tile, with the back button and the
content the wrong shape until they close and reopen it. Nobody would file that as "responsive design
is broken"; they would file it as "Settings is glitchy".

**The uniform rule this argues for (and the answer to "how does this even happen").** A surface must
derive its own breakpoint **from the rect it is laid out in, on every layout pass**, with hysteresis so
it doesn't flutter, and the host may only *override* it (for docking, display edges, workspace
constraints) — never supply the initial truth. The toolkit already has the right machinery: `NavMode`
with per-threshold hysteresis (`nav_shell.rs:39`) and the explicit promise that "there is no width,
breakpoint … to pass" (`:57`). The problem is that the *decision* is still app-supplied
(`.mode(NavMode::Push)`, `settings_app.rs:693`) and partly cached.

**And the sweep says this is not one app.** 15 apps hardcode their own width thresholds — `imager` 8,
`settings-core` 7, `slides` 5, `video-editor` 14 — while only six use the shared `NavMode`
(`activity`, `clocks`, `remote`, `settings-core`, `shortcuts`, `tasks`). So the system currently has
one good uniform mechanism and twenty private ones, which is `P5` (Forty Grammars) wearing a layout
costume.

**Fix, in full.**
1. One `Breakpoint` value (`Compact`/`Medium`/`Wide`) owned by the toolkit, computed by each surface
   from its own laid-out rect with hysteresis, re-evaluated **every** layout pass.
2. Host hint becomes an override with a stated reason, never the seed.
3. Migrate the 15 literal-threshold apps onto `NavMode`/`Breakpoint`; a policy check fails a build on a
   new literal width comparison outside the tokens.
4. A harness test asserts `render(construct-at-W) == render(construct-at-default-then-resize-to-W)` for
   every app and every preset. That single equality is what caught this, and it catches the next one.

## 4.3 T2 — notable — Every capture renders with a fallback font, silently

Every invocation above logs:

```
ERROR tilo_ui::font_catalog: Tilo font catalog unavailable
  errors=[FontCatalogError { path: ".../target/debug/fonts", problem: "no font catalog directory" }]
```

and then renders anyway. The catalog is looked up beside the executable and in `/run/tilo-fonts`
(`font_catalog.rs:109-114`), neither of which exists in a build tree.

**Why this matters:** text *metrics* decide what fits. Every overflow, truncation and reflow result
in this audit — and in every PR that posts a frame — was measured with a substituted typeface. A
label that fits here may not fit in the shipped image, and vice versa. That silently invalidates the
one class of claim a screenshot is supposed to support.

**Fix.** A capture that cannot find the production font catalog must **fail**, not warn. When it
can't (developer laptop), it must print the substitution on the frame itself or in a footer, so no
reviewer can mistake the frame for production typography. This is a prerequisite for `M10`.

## 4.4 T3 — medium — Pseudo-localisation wraps some strings twice, corrupting the very signal it exists to give

At 390×844 with `--pseudo-locale`, the Settings title reads `[Nétwórk]` (wrapped once — correct)
while section headers read `[[Wíí-Fíí]]` and `[[Wíírééd]]` (**wrapped twice**). The same strings in
the shipped locale are plain section titles.

**Why it matters:** two reasons, one of which is a real user bug and one of which is a broken review
tool.

- A string that passes through the localiser twice is a **localisation plumbing defect**. With the
  pseudo-locale it is cosmetic; with a real locale it is the class of bug that mangles interpolation
  (`{ $count }` formatted twice) and plural selection.
- The pseudo-locale exists to *detect truncation*. Double brackets in the signal mean the reviewer
  cannot tell "this string is long" from "this string is processed twice", and the extra brackets add
  width that is not real — so truncation reports for those rows are wrong in both directions.

**Fix.** Localise at the point of formatting, once, and add the check the pseudo-locale is for:
a build test that fails if any rendered string contains the pseudo-locale's own delimiters.

## 4.5 T4 — minor — A button clips its own translated label

`calc-pseudo.png`: the Functions panel's add button renders `+ [ÅÅdd fünctïön]` with the text
overrunning the chip's right edge — the chip is sized as if the label were the English "Add
function". The English frame fits exactly, which is why nobody noticed.

**Why it matters:** the button's width is not derived from its measured label, so it holds only for
the string it was designed against. Any translation longer than English — German and Finnish
routinely are — clips. This is the visible half of a class that `M5` (one declaration, generated
surfaces) removes structurally: a control measures its own label or it is a bug.

**Fix.** Size from measurement, and add "the painted label must fit its rect" as a capture
assertion. It is the cheapest possible gate and it would have caught this without a translator.

## 4.6 T5 — high contrast is genuinely good on charts (and a suspicion I had was wrong)

I expected to report that the high-contrast theme leaves the graph's grid and axis ink behind. It
does not. Measured from the frames:

| Element | Dark | High contrast | Verdict |
|---|---|---|---|
| Axis labels ("6", "5", …) | **6.70 : 1** | **12.94 : 1** | both pass AA for small text; HC nearly doubles it |
| Axis line | 4.42 : 1 | 4.30 : 1 | both clear the 3 : 1 graphical floor |
| Grid lines | 1.34 : 1 | 1.22 : 1 | decorative, deliberately recessive — not a defect |
| Function-row text | — | **20.82 : 1** | HC meant it |

So the chart is legible under HC, and the faint grid is a design choice, not an oversight. Recording
this because it is exactly the kind of claim that gets "fixed" by an over-eager reviewer: **do not
strengthen the grid.** The only nit worth a look is that the *axis line* is marginally weaker in HC
than in dark (4.30 vs 4.42) — a theme that promises more contrast should never reduce an existing
ratio, even slightly — but it is inside the noise of antialiasing.

## 4.7 T6 — positive — Settings at every narrow combination, except through the matrix

The width/scale/locale matrix, done by hand, is a model of narrow behaviour:

| 390×844 | Result |
|---|---|
| default text | narrow pane, 2-column theme grid, everything readable |
| **text scale 1.5** | narrow pane, grid still 2 columns, labels wrap instead of clipping, `Interface size` reflows to a full-width control |
| **pseudo-locale** | narrow pane, long labels truncate with ellipsis and the rows keep their structure |

And 380/400/420/440 are all clean, with the theme grid stepping 2 → 3 columns at the wider sizes.

This is what `M6` should mean everywhere: as the surface narrows it changes *arrangement* and
*navigation*, never what you can do. It also strengthens `VIS-07` in Volume I: the narrow model is
right; the matrix is what is wrong (`T1`).

## 4.8 T7 — medium — Launch latency cannot be measured, so "it feels slow" cannot be settled

`--first-frame` constructs the app, lays out and draws the first frame, and is documented to "print
where the time went". In this build it prints **nothing** and exits 0:

```
$ ./target/debug/calculator --first-frame; echo $?
0
# only the font-catalog ERROR
```

The marks come from `tilo_startup_metrics` (`launch.rs:1028-1046`), which is evidently not wired in
this configuration. The consequence for this audit is honest and worth stating: **I have no
measurement of launch or interaction latency for any app**, so every judgement here is about what is
*shown*, never about how fast it arrives. A design gate that cannot measure the first frame is
missing the metric people complain about most.

## 4.9 Monitor — the same word means two different things, and the one that explains it is in a side panel

Rendered wide (1920×1080) and at 390×844. Two findings, one positive.

**`T8` — medium — "Memory" is two measures.** The Memory *card* reads `8.6 GiB` (system memory in
use). The Memory *column* on the first row reads `10.4 GiB` (that process **with children**). Both are
labelled "Memory", both are on screen at once, and the qualifier that reconciles them exists only in
the inspector panel that appears after selecting the row ("Memory with children: 10.4 GiB"). Until you
select it, the app appears to report a process using more memory than the machine is using.

This is `P1` in its purest form, and the fix is not a units suffix on every row: it is that **a value
must carry its measure**, the way `M1` requires. "Memory (with children)" in the header, or two
columns, or the card labelled "In use" — any of these closes a question the person currently cannot
answer without opening a panel they had no reason to open.

**`T9` — minor — the sparkline cards state a rate and show a shape with no scale and no period.**
"Network 6.7 MB/s" sits above a squiggle with no time span, no axis, and no way to read a past point
(verified in both the wide and narrow frames: there is no label, tick or readout anywhere on the
card). "6.7 MB/s" *right now* and the curve covering *some* window are different claims, and the person
cannot tell which the curve is. Even a right-edge "last 60 s" caption would make the shape readable;
scrubbing it to read a past value is the version that would make Monitor feel like an instrument
instead of a wallpaper.

**Positive — the destructive pair is right.** The inspector offers **End process** (neutral) and
**Kill process** (red text, not a red slab), side by side, with a confirmation behind Kill. This is
exactly the shape `M4` asks for: the graceful verb is the default, the irreversible one is marked, and
the difference is in words rather than in colour alone. Volume I's earlier claim that Kill had no
graceful alternative was wrong; this is the frame that disproves it.

**One nit worth naming because it will be repeated everywhere:** the inspector panel is flush to the
window's right edge with no margin, so on a maximised window its text can sit against the frame. In a
system that tiles by design, the outer margin is the only thing separating content from the edge of
the world; it should hold at every width.

## 4.10 What this round changed in my own conclusions

Two corrections, both from testing rather than reading:

1. **`VIS-05` needed three passes and I was wrong twice.** First pass: "Settings is clipped at
   narrow width" — from an old artifact. Second pass: "dead, the app has a narrow mode" — from one
   clean render. Third pass: *both*, in a precise form — the app is correct when it is told how wide
   it is, and produces a clipped desktop layout when it is not (`T1`). The lesson is not that the
   first observation was silly; it is that one render settles nothing. You need the *same state
   reached two ways* before you know what you are looking at.
2. **Two suspicions of mine were wrong** and would have become bugs if I had filed them: the
   high-contrast chart (`T5` — HC nearly doubles axis-label contrast, 6.70 : 1 → 12.94 : 1) and
   Monitor's stat colours disappearing at narrow width (they never had colours in this build; the old
   artifact did). Both were eyeball judgements that a measurement or a second comparison corrected.

That is the case for `M10` in one paragraph: reading finds classes, but only rendering finds the
truth about a specific frame — and only *both* paths, compared, find the lies in your own evidence.

---

## 4.11 T10 — notable — Captures are not reproducible to the byte, so no screenshot gate may demand it

Writing the parity check for `T1` produced this on its second run: the **same
command, same binary, same arguments** wrote two different PNGs.

```
five identical runs of `calculator --shots --viewport 768x1024`:
  d0994809  d0994809  4e601e16  d0994809  4e601e16
```

The two frames differ in **one pixel** — `(509, 330)` — an antialiased sample of a
graph curve landing either side of a pixel boundary. So the rendering pipeline is
reproducible in structure and not in ties.

**Why it matters more than a cosmetic flake.** Every gate I proposed in `M10`
compares frames. A gate that demands byte equality fails about half the time on a
*correct* app, and a gate that flakes is a gate somebody turns off — which returns
the codebase to the state this audit found. Worse, it retroactively explains
something in my own method: `T1`'s conclusion had to rest on differences of
hundreds of thousands of pixels (277,493 at phone), because a small difference
could not be distinguished from a tie. The evidence was valid; the instrument was
not precise enough to have claimed anything smaller.

**What a frame gate must therefore be.** Perceptual, with a *stated* tolerance,
and the tolerance has to be small enough that a real layout difference is orders of
magnitude beyond it. `tools/ci/capture_parity.py` (written during this pass) uses
64 pixels: 1 in 12,000 of a 768×1024 frame, and 4,300× smaller than the Settings
difference it is there to catch. The number is in the file with the reason, not
implied.

**The underlying defect is real and worth its own fix**: a graph that samples a
curve should produce the same samples for the same inputs. A single tie is a
floating-point ordering question inside the sampler, and reproducibility is worth
having for its own sake — a person comparing two screenshots of the same function
should not find them different.

## 4.12 T11 — blocker for the gate — A capture's result depends on what captured before it

The parity check found this while being calibrated, and it is the most consequential thing in
this round, because it undermines every frame anybody has ever compared.

Four captures of the same app at the same size, all `--shots --viewport 1920x1080 --theme dark`:

```
shots, then matrix   -> 1ba06cb8eb0d
matrix, then shots   -> b8248230f4f5
matrix (first)       -> 80a31e3b80a1
matrix (after shots) -> d83285160258
```

Same binary, same arguments, different output directories — and **all four differ**. So a frame is
not a function of (app, size, theme, scale, locale). It is a function of that *and* whatever ran
before it in the same machine state.

**What has to be leaking.** Something an app writes when it is resized or captured is read back by
the next capture: a window size, a pane width, a collapsed sidebar, a last route. The capture
harness seeds a private profile per capture (`launch/capture_settings.rs`), so the leak is either
outside that profile or written after seeding. Either way the rule is clear — a capture must not
be able to observe a previous capture, and an app must not persist a layout decision the person
did not make.

**Why this matters more than T10.** `T10` (a one-pixel tie) means a byte gate cannot be exact.
`T11` means a frame cannot be *trusted at all* as evidence until runs are isolated: two reviewers
can look at two different frames of the same screen and both be honest. Every visual claim in this
audit, including my own, inherits that doubt; the ones that survive are those where the difference
was large enough to swamp it (the Settings layout differed by 277,493 pixels — that is not a
persistence artifact).

**What it changes about `M10`.** The gate's first assertion is no longer "frames match"; it is
"runs are isolated and frames are a function of their inputs". Until that holds, `tools/ci/
capture_parity.py` is a diagnostic to run by hand and read, not a check to enforce, and this
document says so rather than shipping a gate that fails for reasons nobody can name.

# 5. Apps: the promise, the gap, and the honest fix

## 5.0 The standard, and two things this section refuses to do

The question is not "does this app deserve to exist" or "should we merge these two". The question is:
**does the app fulfil the job a person opens it for, at the standard the rest of the system sets?**

Two refusals, stated up front because they are the easy way out and both are wrong:

- **No pruning of ambition.** An app that promises something and cannot deliver is not fixed by
  removing the promise. Firewall either enforces or it is a defect to repair — not a feature to
  delete. The same holds for anything else here that is half-built.
- **No merging by surface similarity.** Two things that both have a composer are not the same job.
  Merging by shape produces a third thing that is bad at both jobs. Where several apps share
  *mechanics*, the answer is a substrate underneath them, and the apps stay distinct.

Deletion appears exactly once in this section, and only for genuine redundancy: two surfaces doing
the same job for the same person, where keeping both makes each worse.

## 5.1 Two conversations, and one substrate under them

**The distinction is the primary object.** Chat's primary object is a *channel* — a room with a
history, a membership and a mood; you go there to see what a group is doing. Messages' primary object
is a *person* — a thread that may arrive by SMS, Signal or WhatsApp; you go there to talk to someone.
Chat is many-to-many and public-ish; Messages is one-to-one and private. Neither is a subset of the
other, and a person uses both in the same hour for different reasons.

So: **the apps stay separate; the mechanics they both need live below both.** Today each invents its
own, which is why the same gesture behaves differently:

| Substrate piece | Chat today | Messages today | What it should be |
|---|---|---|---|
| Day/time structure | one divider for the oldest loaded message, rows show a clock and no date (`chat/render_chat.rs:831-846`, `:1201`) | **no timestamps at all** (`messages/history.rs:236-259`) | one message-list primitive with day dividers, per-message time on hover/focus, and a "jump to date" affordance |
| Attachments | painted thumbnail, cannot be opened, enlarged or saved (`APP-chat-02`) | none | one attachment surface: inline preview → full-screen viewer → open in Files → save |
| Delivery/failure | refusal restores text (`events.rs:1701-1707`) — good | `Submitted`/`Acknowledged`/`Delivered`/`Read`/`Uncertain` — good | one delivery-state vocabulary rendered identically in both |
| Drafts | persisted per channel (`composer.rs:32-40`) but invisible in the sidebar | **lost on close** (`APP-messages-03`) | drafts persisted on the person's record, and *shown* wherever the thread is listed |
| Composer keys | Enter sends, Shift+Enter newline | **no keys at all** (`APP-messages-02`) | same composer widget, same rules, both apps |
| Triage | unread counts, mention alerts | per-thread state | same unread/attention contract, feeding `M3` |

**The fix is not a merge; it is `ui-core` growing a conversation substrate** that both apps draw,
which is the same pattern the toolkit already uses for lists, tables and empty states. Consequences
a person feels: a screenshot in Chat opens full size; a message in Messages has a time; a draft in
either app is visible where you left it; Enter sends in both.

**And the two apps then get their own proper investments**, which today they don't have because the
mechanics eat the budget:

- *Chat* needs a date-aware history, an in-channel search that reaches beyond the loaded window, and
  mention notifications that open the right message (`APP-chat-05`).
- *Messages* needs its transports to be first-class and visible — which account a thread is on, the
  compose-and-send flow, and history that pages (`APP-messages-04`).

## 5.2 The trust ring: Firewall, Passwords, TOTP, Publisher, Permissions

These are the apps where being wrong costs the person something they cannot get back. All five are
currently *honest-looking and incomplete*, which is the worst combination.

**Firewall — make it a firewall.** In order of what a person needs:

1. **It enforces.** The capability is granted (a mediated, person-authorised connection to the
   firewall manager — see `P9` and `M13`), so the switches and Apply are real. No hidden dry-run, no
   env var in the product path.
2. **It shows what it blocked.** Today there is no view of dropped traffic at all (`APP-firewall-03`).
   This is the difference between a firewall you operate and a firewall you hope about: without it the
   person cannot tell whether their rules work or what is hammering the machine. A "Blocked" page,
   grouped by rule, with counts over time.
3. **It lets you edit a rule** instead of delete-and-retype (`APP-firewall-04`), and it confirms the two
   global switches that can cut your own session (`APP-firewall-05`).
4. **Fields ask for the right input.** A CIDR field that raises a numeric pad (`APP-firewall-02`) is
   wrong at the level of "what is this field": it is an address, not a number.
5. If a *preview* is wanted, it is a user-chosen **diff**: "these 4 rules would change — apply /
   discard". Never a mode where applying silently doesn't.

**Passwords — it is a vault, so it should behave like one.** The bones are good (timed clipboard,
timed reveal — `actions.rs:51-62`). Missing: the item's primary action is dim text while lesser
actions get buttons; deleting lives only inside the editor; and there is no *health* surface in the
place you look (the "Password review" row exists but the item itself never says "weak", "reused",
"old"). A vault's job is "help me not get owned", which is a review loop, not a table.

**TOTP — privacy is the feature.** Codes are painted on every row, always (`APP-totp-01`). The app
should be *unreadable at a glance by design*: a reveal-per-row or a lock, plus a clock-drift warning
(`APP-totp-02`), because a code that every server rejects with no explanation is indistinguishable
from a broken app. And the clipboard lifetime should follow the account's period, not a fixed 30 s.

**Publisher — it is a workflow, and the workflow is missing three of its five steps.** The promise is
"make my app installable and attributable to me". The app has key creation, proof generation and
namespace application; it does not have: the **places** to publish the proof (the DNS name and the
`.well-known` URL exist in `proof_places` and are printed by the CLI, never by the app —
`APP-publisher-02`), a **real key backup** (the warning is dead copy naming a file the app never
writes — `APP-publisher-01`), and a **status timeline** ("applied 3 minutes ago · waiting for
review · live"). Without those it is a form that produces a string; with them it is the thing that
makes an app *yours* in front of other people.

**Permissions — the one surface whose whole job is trust, and it is buried.** A person cannot answer
"what can this app see?" from the app they are worried about. The system already knows; the answer
should be reachable *from the app* (a badge in the window bar → the app's permissions, live), not only
from a settings list of every app. Trust at the point of doubt, not at the point of configuration.

## 5.3 The long-work ring: Store, Backups, Containers, Disk usage, Print centre

All five are clients of `M2` (Jobs). Each also has one job-specific thing missing that `M2` cannot
give it:

- **Store** — installs must be watchable and cancellable, and the Store must stay usable while one
  runs (`APP-store-01`). Job-specific: **size on the row and a total** (`APP-store-02`), because "what
  is this costing me" is the second question after "can I install it", and a per-row remove so
  clearing five apps does not mean five pages (`APP-store-03`).
- **Backups** — the whole app currently reports constants as measurements (`APP-backups-01`) and
  offers a schedule it does not have (`APP-backups-02`). That is the largest single honesty debt in
  the system. Job-specific: a **retention preview** ("lowering this removes these 3 snapshots, dated
  …", `APP-backups-06`) and **why a run failed** (`APP-backups-07`). A backup app's only real feature
  is trust; every one of these is trust.
- **Containers** — live logs, not a snapshot (`APP-containers-01`); in-flight states on verbs; and the
  confirmation should name the container, not its id (`APP-containers-02`).
- **Disk usage** — a scan is a Job: bytes and files seen, a Stop, and a result you can navigate. The
  cancel machinery exists but is only reachable through Back, which reads as navigation
  (`APP-disk-usage-01`).
- **Print centre** — the form is three fields; the job is "print pages 3–5 of this, double-sided, A4"
  (`APP-print-centre-01`). The queue, cancel and friendly fault messages are already right — this is
  genuinely the best-behaved admin app in the tree and should be the reference for the others.

## 5.4 The creation ring: Calculator, Sheets, Slides, Canvas, Notes, Word processor, Typst, Code editor, PDF, Hex, Books

For each, "system quality" means the same three things: the thing you make is *findable*, the thing
you make is *reversible*, and the thing you make can *leave the app*.

| App | The promise | The gap that breaks it | The proper fix |
|---|---|---|---|
| Calculator | "type a sum, get an answer, take it with you" | a narrow window deletes 22 function keys with no door (`VIS-01`); the result cannot be copied (`APP-calculator-01`) | a `2nd`/fx page for functions (a *door*, `M6`) and the result as a first-class copy target — every display value should be copyable |
| Sheets | "a grid I can think in" | no cell context menu (`APP-sheets-01`), no find/replace over a workbook (`APP-sheets-02`), structural edits as hard cuts | the shared edit menu (`M5`) + the shared find matcher (`DUP-01`) + find-as-navigation ("match 4 of 12, jump") |
| Slides | "a deck I can present" | cannot zoom the canvas (`APP-slides-01`), cannot copy/paste elements or slides (`APP-slides-03`), Ctrl+P starts a show (`APP-slides-02`) | the shared zoom model (`DUP-02`), the shared clipboard, and the chord grammar (`M5`) |
| Canvas | "draw something precise" | its own header row instead of the bar (`CHROME-03`); no per-artwork zoom readout in the bar | declare slots; the artwork title becomes the bar's editable location |
| Notes | "a note I can navigate" | no heading outline for a long note (`APP-notes-01`), scroll lost when switching (`APP-notes-02`) | the outline is a *view of the document*, so it belongs in the shared block editor, not in Notes |
| Word processor | "a document I can page through" | PageUp/PageDown do nothing (`APP-word-processor-01`); no "page 3 of 12" and the only page label is suppressed by footers (`APP-word-processor-02`) | pagination readout as a first-class `StatusReadout` with a typeable page number; paging keys in the shared text-editing core |
| Typst | "edit source, see the PDF" | no find in the source (`APP-typst-01`), no unsaved indicator (`APP-typst-02`), its own command bar (`CHROME-02`) | `Findable` on the shared code editor (which is the actual missing piece — it has none), `edited` on the bar, slots declared |
| Code editor | "the editor I live in" | no zoom or font size (`APP-code-editor-01`) | `ContentZoom` (`DUP-02`) — text size is the single most-requested setting in any editor |
| PDF viewer | "read and mark up a document" | always reopens at page 1 (`APP-pdf-viewer-01`) | a persisted reading position — the ebook already proves the record supports it |
| Hex editor | "look at and change bytes" | no copy/paste of bytes (`APP-hex-editor-01`) | clipboard in the current focus mode, one undo step |
| Books | "read a book" | re-implements the window bar twice in-body (`CHROME-01`) | declare slots; keep the reading typography |

The pattern across this ring: **the missing pieces are shared, and each app is waiting on the
toolkit.** Find, zoom, clipboard, outline, pagination readout, unsaved state. That is not eleven
projects; it is five toolkit capabilities and eleven declarations.

## 5.5 The reading ring: Browser, Tube, Maps, Weather, Feeds, Photos, Music, Video

- **Browser** — a real browser with a shopped-short tail: no bookmark suggestions in the address bar
  (`APP-browser-02`), no per-visit history removal (`APP-browser-03`), no tab muting (the desktop's
  job, but nothing asks for it), and a half-translated UI (`APP-browser-04`). The engine work is
  impressive; the *library* is the weak half, and libraries are where people live.
- **Tube** — resume (`APP-tube-01`), working quality selection (`APP-tube-02`), playback speed
  (`APP-tube-03`), captions (`APP-tube-04`). A video app without resume, speed and captions is
  missing the three things people actually use. These are not polish; they are the product.
- **Maps** — one route (`APP-maps-01`), raw transport errors shown to people (`APP-maps-02`), no way to
  stop an offline download (`APP-maps-03`), and an untranslated UI (`APP-maps-04`).
- **Weather** — no accessible hourly/daily (`APP-weather-02`), no "how old is this forecast"
  (`APP-weather-03`), millimetres under Imperial (`APP-weather-01`).
- **Feeds** — a failed subscription silently keeps looking healthy (`APP-feeds-01`). One line of row
  state would fix it.
- **Photos** — no full screen, no slideshow (`APP-photos-01`). For an app whose job is "look at a
  picture", showing it at maximum size is the core loop.
- **Music / Video player** — the transport is the reference implementation; Video player needs what
  Music has: persisted volume/rate and resume (`APP-video-player-01`).

## 5.6 The admin and developer ring

- **Git client** — the four things a git GUI exists for: switch branches safely (`APP-git-client-01`),
  see what a commit changed (`APP-git-client-02`), resolve a conflict (`APP-git-client-04`), and do it
  all from the keyboard (`APP-git-client-03`). It currently does the *diff* half beautifully and none
  of the *decision* half.
- **Diff & merge** — cannot save its merged result (`APP-diffmerge-01`), no keyboard accept
  (`APP-diffmerge-02`), no folder compare (`APP-diffmerge-03`). The first is fatal: the tool completes
  the job and then cannot hand it back.
- **Db client** — cancel a query (`APP-db-client-01`), copy a cell (`APP-db-client-04`), a real "this
  result was cut" state (`APP-db-client-02`), and translation (`APP-db-client-05`).
- **API tester** — cancel/timeout (`APP-api-tester-01`), replay with the full request
  (`APP-api-tester-02`), copy the response (`APP-api-tester-03`).
- **Terminal** — the grid is legitimately app-owned and excellent; it needs the *bell* (`APP-terminal-02`)
  and persisted copy-on-select (`APP-terminal-01`), both of which are system-level preferences it
  currently keeps in its own head.
- **Logs** — Follow must follow (`MOTION-07`); entries must be copyable (`APP-logs-01`).
- **Monitor** — the measure labels (`T8`), the missing period/scale on the cards (`T9`), and the
  side-panel margin (`T8` nit).
- **Disks** — mount, then a way to get there (`APP-disks-01`). The destructive path is already the
  best-designed in the app.
- **Windows (appliance)** — translate the status sentences (`APP-windows-01`); its lifecycle gating is
  correct.
- **Remote** — cancel a hung connect (`APP-remote-01`), confirm a disconnect (`APP-remote-02`), and
  file transfer (`APP-remote-03`).
- **Chip design** — a canvas you cannot zoom or fit (`APP-chip-design-01`) and no operator list
  (`APP-chip-design-02`).
- **Poker / Speed test** — small, and each has one honesty gap (`APP-poker-01` enables a button that
  refuses for 30 s with no countdown). Speed test is otherwise a model of stop-and-keep-results.

## 5.7 The system ring: Files, Settings, Activity, Clocks, Calculator, OSK, Themes, Shortcuts

- **Files** — search the folder you are in (`APP-file-explorer-01`), persist column widths
  (`APP-file-explorer-02`). Everything else here is strong; conflict handling and transfer cancel are
  the references for the whole system.
- **Settings** — the narrow model is right; what it needs is that the *app knows how wide it is*
  (`T1`), no internal paths on screen (`APP-settings-01`), status as words not colour
  (`APP-settings-02`), and a real "changed what?" confirmation.
- **Activity** — text search (`APP-activity-01`), before/after values in Compare (`APP-activity-03`),
  and it becomes the ledger that `M9` shows.
- **Clocks** — snooze (`APP-clocks-01`), edit an alarm (`APP-clocks-02`), next-ring time
  (`APP-clocks-03`). Its once-per-device delivery ledger is exactly the right thinking.
- **OSK** — a size preference (`APP-osk-01`), autocorrect separate from suggestions (`APP-osk-02`), and
  a typing language independent of the key layout (`APP-osk-03`).
- **Themes** — import (`APP-themes-01`), and a preview that shows a *list* and a *dialog*, not just a
  desktop (`APP-themes-02`). Its refusal to ship an illegible theme is the best guardrail in the tree.
- **Shortcuts (automation)** — a filter on the action picker (`APP-shortcuts-01`), duplicate
  (`APP-shortcuts-02`). This app is quietly one of the most ambitious things here and its blockers are
  small.

## 5.8 What is missing — three system surfaces, not three apps

The gaps that recur across the audit are not app-shaped. They are system-shaped, and that is why no
app owner could have fixed them:

1. **Jobs** (`M2`) — the single biggest missing surface. Nine apps are improvising it.
2. **Needs you** (`M3`) — the attention model. Eleven sources currently shout into different holes.
3. **Present** (`M-ish, §3.12 when written) — a *mode* any content app can enter: content fills the
   screen, chrome withdraws, one key to advance, one key to leave. Slides, Photos, PDF, Video, Books and
   Maps all currently implement "almost fullscreen" separately or not at all.

None of these belongs inside an app. Each is a thing the person should be able to reach from anywhere,
which is the definition of a system surface.

## 5.9 What I would not do — including two things I said earlier and take back

Recorded so the record is straight:

- **Not** merging Messages into Chat. Different primary objects (person vs channel); merging makes both
  worse. Unify the substrate (§5.1).
- **Not** folding Publisher into a settings pane. It is a workflow with waiting and status; settings
  panes hide waiting. Fix the workflow.
- **Not** removing the Firewall until it can enforce. Fix the capability (`P9`, `M13`). Removing a
  promise is not a design decision, it is an admission.
- **Not** merging Disks + Disk usage + storage settings either, for the same reason: partitioning is an
  administrative act on hardware, usage is a browse through your own stuff. They look similar from a
  distance; they are not the same job. What *is* broken is that a person cannot find either from the
  other — a cross-link ("how much space is left?" from the formatter; "which disk?" from the usage
  view) fixes discoverability without collapsing two jobs into one.
- **Not** deleting the small apps. Poker, Calculator, Weather, Clocks: these are the ones that make a
  desktop feel inhabited. Their defects are small and their presence is not the problem.

---

# 5b. Where my own design was wrong

An audit that only criticises other people's choices is a list, not a design. So, having
written thirteen moves, here is what is wrong with them — including two where today's work
taught me something I could not have known while writing.

## 5b.1 — Undo does not replace confirmation for effects outside the document

`M4` says "most confirmation dialogs become undo". That is right for a note, a sheet, a file
rename. It is **wrong** for anything that acts on the world before you can undo it: a firewall
rule that drops your own session, a disk format, sending a message, deleting a secret. Undo of a
network cut does not restore the twenty seconds in which a build failed and a colleague lost
their session.

The corrected rule: undo replaces confirmation only when the effect is contained (it changes
something the app owns), instantly visible, and reversible with no external consequence. Every
other destructive act keeps a confirmation, and the confirmation stays short because the
irreversible list is short and known.

## 5b.2 — A "door" is a better outcome than a lost key, and still a bad outcome

`M6` says a control that does not fit becomes a named overflow door. That is true as a floor. But
a door over twenty-two calculator functions at 390 px is not good design — it is a cramped layout
apologising for itself. The better answer is often to **change the layout so nothing has to hide**:
a keypad that becomes the screen rather than a squeezed half, a function page that is a *place*
instead of an overflow. `M6` should be read as "a capability may never vanish silently", never as
"hiding is acceptable if you name it".

## 5b.3 — Three surfaces where two would do

Taken together, `M2` (Jobs), `M3` (Attention) and `M9` (the ledger) give a person three places to
look, which is one more than the two questions they actually ask. The honest simplification:

- **Needs you** — what asks something of me. (`M3`)
- **Activity** — what happened and what is still happening, in one scroll: a running install is a
  row that is not finished yet, a finished backup is a row that is. (`M2` + `M9`)

Jobs should not be a third window. A long job additionally shows *where it was started* — a bar
slot in the app that began it — and that is enough. This is the version I would build.

## 5b.4 — `Fact` should not stamp every number

`M1` says a number carries its measure and its as-of. Taken literally, every screen grows a
timestamp and the person learns to ignore all of them. The rule needs a second half: show the
as-of only when the value can be stale *and* the person can act on the staleness ("3 minutes ago"
next to a forecast is useful; next to a file size it is noise). Elsewhere the freshness is the
design's job: push the update, or do not show the number.

## 5b.5 — Baselines are where fixes go to die

The strongest thing today's work taught me. The repository already had the checks I proposed
(`policy/app-toolbars`, `policy/no-transient-cards`, `policy/motion-library`, `policy/primitives`),
each with a baseline file whose header says "counts only shrink". Those files hold **179**
primitive violations, **10** toolbar violations, and the one I added recorded **63** breakpoint
comparisons. The checks are real and they work — and the debt they hold is the audit's findings,
frozen in amber. A baseline is a promise to fix something later, made by people who will be busy.

So `M10` needs a rule I did not write: **a baseline entry has an owner and an expiry**. When the
entry expires, the check fails on it by name. Without that, an enforcement regime becomes a
monument to the defect it was built to prevent, and a reviewer learns to see "baselined" as
"decided". This applies to the 63 entries I added today.

## 5b.6 — Uniformity of rules, not of surfaces

`P5` (Forty Grammars) blamed variety. Some of that variety is correct: a calculator and a git
client *should* look and behave differently, and the Desktop Metaphor is not improved by making
them share a command surface. What must be uniform is the *rules* — what Enter does, what
`Ctrl+Z` means, where search lives, how a failure is worded, what a chord may never do. `M5`
should be read that way, and not as a licence to generate one enormous menu for everything.

## 5b.7 — Thirteen moves is twelve too many to remember

If a team can hold three, they are:

1. **Truth** (`M13` + `M1`): nothing claims what it has not verified, and a missing capability is
   stated rather than faked.
2. **Undo** (`M4`): the person can always go back, so most fear disappears and most dialogs go.
3. **Attention** (`M3` over `M2`/`M9` as above): one place that says what wants them.

Everything else — motion, chrome, chords, doors, presence, present mode — is craft: it makes the
product feel finished, and none of it matters if the first three are wrong.

## 5b.8 — Nine causes are closer to five

Read again, they compress:

- `P1` Polite Lie and `P3` Raised Hand are one cause: **the interface is allowed to claim things
  the system has not verified** (a state, a severity, a privilege).
- `P5` Forty Grammars and `P7` Motion by memory are one cause: **a cross-cutting decision has no
  single owner**, so it is re-made per surface.
- `P2` Vanishing Capability, `P6` Chrome by committee and `P8` Jobs without bodies are one cause:
  **an app can opt out of the system's guarantees**.
- `P4` (undo as a feature) and `P9` (capability by coincidence) stand alone.

That is four causes and a residual: *the product does not have one owner for truth, one for
consistency, and one for the person's time.* Stated that way it is actionable by one person
instead of thirteen.

## 5b.9 — Every story I wrote ends in failure

Nine realistic people, nine failures. That is a rhetorical choice and it is a methodological
hole: I never checked what Tilo does *better* than the alternative, so I cannot tell anyone what
to protect. Volume I's Appendix B lists what is genuinely good (imager's destructive flow,
code-editor's unsaved handling, files' conflict and trash handling, the theme guardrail, Settings'
narrow pane, the shell's window menu and notification keyboard handling) — but no story exercises
them, so the redesign risks breaking the best parts while fixing the worst. A tenth story, of
someone whose work goes *right*, is missing and should be written before any of this is built.

## 5b.10 — Which of these moves is evidence, and which is taste

Stated plainly, because a reader deserves to know what is measured:

| Evidence-backed | Inference | Taste |
|---|---|---|
| M1, M2, M3, M4, M6, M9, M10, M13 (defects counted, reproduced, or rendered) | M5, M7, M8 (systematic, but the benefit is judged from the code and the frames) | M11, M12 (presence and present mode are good ideas for a collaborative desktop; nothing here measures how many people collaborate, or present) |

The last row is not a reason to skip them — they are the two things nobody else can copy — but
they should be built *after* the measured fixes, and they should be built small enough to abandon.

# 6. The bets, in order — and the one habit to stop

Six bets, then one habit. Each bet has a first slice measurable in days, and a signal that says it worked. They are
ordered by *felt change per unit of work*, and the ordering is deliberately not "bugs first".

## 6.1 — Truth, and the machine that keeps it (M13, M1, M10, and `T2`)

**Why first.** The system currently says things that are not so — a firewall that does not fire, a
backup that is always healthy, a schedule that does not exist, a status that cannot change, an
"Apply" that applies nothing. Everything else in this volume is an improvement; this is the difference
between a system you can trust and one you have to check.

**First slice.** (a) The capability door for the three privileged apps that can lie hardest (Firewall,
Backups, Containers) — granted / grantable / unavailable, no inert controls. (b) `Fact` with an as-of,
adopted in those same three apps. (c) The design gate's first four assertions, and the font catalog
made fatal in captures.

**Signal it worked.** A reviewer can open any of those three apps, cover the title bar, and still tell
what is real and how old it is. And a PR that adds a faked status cannot merge.

## 6.2 — Remove fear and noise (M4, M9)

**Why second.** These two *delete* UI, and deletion is the only change every person notices
immediately and approves of. Undo as a seam means most "are you sure?" disappears; the ledger means
most toasts disappear.

**First slice.** The undo seam on three surfaces with real history (a note, a sheet, a file
operation), plus the irreversible list and one confirmation style. The ledger reading from
`activity`'s existing history, with "Backup finished / 3 files moved / Pushed" writing to it instead of
throwing a toast.

**Signal it worked.** Two modals deleted from a real flow without anyone feeling less safe — and one
person saying "wait, I can scrub backwards?".

## 6.3 — The two missing surfaces (M3, M2)

**Why third.** These are the biggest *additions*, and they are the two questions a person asks all day:
"what wants me?" and "is the thing I started still going?". They should not start before 6.1, because a
Needs-you stack full of unowned, unverified facts would just be a louder version of today.

**First slice.** Jobs with four clients (Store install, backup run, container pull, attachment upload),
cancellable, surviving their windows, failures landing in Needs-you. One chord to Needs-you; every row
with an owner and one action.

**Signal it worked.** Nobody closes a window to "let it finish" any more, and the bell is a place you
can act from rather than a number you learn to ignore.

## 6.4 — The coherence layer (M5, M6, M7)

**Why fourth, and why together.** These three are the same move applied to commands, layout and
motion: declare once, generate the rest, enforce with a check. Doing one without the others leaves the
other two drifting, and all three are mostly *deletion* — six shortcut sheets, four edit menus, pops
and cuts, silent capability loss.

**First slice.** The command declaration feeding one real surface end to end (bar slot, menu, palette,
shortcut sheet, a11y node) — pick one app and make it the reference. `Overflowable` in the calculator
and one table. The eight intents with the equality test for two of them.

**Signal it worked.** A chord learned in one app works in another without anyone being told, and the
shortcut sheet stops being able to be wrong.

## 6.5 — One chrome (M8)

**Why fifth.** It only becomes cheap once `M5` generates commands and `M3`/`M2`/`M4` need a home in the
bar. Doing it first would be a layout refactor with no visible payoff; doing it here finishes the job
the bar was always meant to do.

**First slice.** Books, Typst and Canvas declare slots and lose their own rows; the policy check lands
with them so no fourth app can opt out.

## 6.6 — The differentiators (M11, M12)

**Why sixth.** These are the two moves nobody else can copy, and they should not be built on an
incoherent base — presence over a system that lies about its state would be charming and wrong.

**First slice.** The Presence line on one surface with real collaborative history; Present on Slides
and Photos with presenter notes on the second display.

**Signal it worked.** Someone shows a colleague the scrub, or "what changed since you last looked",
without being asked. That is the only honest test of a differentiator.

## 6.7 — The one habit to stop, starting now

Not a bet — a rule, and it costs nothing:

> **If it is the second app doing it, it goes in the toolkit.**

Volume I is 170 findings, and the overwhelming majority are the *second*, *third* and *ninth* copies of
a thing the platform already knows how to do: progress, notices, save prompts, zoom, fullscreen,
find, breakpoints, edit menus, shortcut sheets, empty states, loading states. Every one of those copies
is a place where the product can disagree with itself.

The corollary, for PR review: a change that adds a *new* per-app mechanism should require a sentence
saying why the toolkit cannot own it. The repo already asks for declared deviations elsewhere
(`allow` attributes, policy exceptions); this is the same discipline applied to interaction.

---

# 7. What landed, as commits

A snapshot, so this document can be read against the tree rather than trusted. Each entry is a
commit on `ux-qol-pass` off `3203631d96`; the finding ids are Volume I's.

## 7.1 The toolkit and the evidence

| Commit | What it settles |
|---|---|
| `1d9ac537f0` | `T2`: a capture fails rather than sets text in the host's faces. The catalog is found at runtime (host input + the prepared cache), and the 27 labels of the conversation substrate moved into the toolkit's catalogues (`8f0f9ebcf4`). |
| `f590f7b922`, `8714432af4` | The parity check, and its floor: a layout difference is counted after a 4× downsample, so a fractional glyph offset is not one. Calibrated against the pre-fix Settings binary: 277,493 → 0. |
| `46192c2bbe` | `T1`: a surface's breakpoint is the rect it is laid out in, every pass. Settings at 390 now draws the same opened narrow and resized narrow; video-editor passes all four presets; `tools/policy/breakpoint_baseline.txt` records the 63 comparisons still deciding for themselves. |
| `7ed9d2ff9e`, `6041e34e55` | `M2`, `M3`, `M9`: a job has a body, an item that wants a person has a row that cannot exist without an action, and the ledger is where an outcome that needs nobody goes. |
| `27aafcdc70` | `M1`, `M13`: a measured fact carries its measure and its as-of, and privilege is granted rather than sniffed. `u…Fact` became `Measured` (`09b5e23979`) because two public types called `Fact` is a trap. |
| `b0e554597b` | The breakpoint check reads code, not string literals (yt-dlp's `[height<=720]` selector was not a layout decision), and three apps declared the capabilities their new code uses. |

## 7.2 The apps

| Commit | Area |
|---|---|
| `8d9632f850`, `9afc58dbff`, `bb81004d3e` | Conversations: one substrate under Messages, Chat, Mail and Contacts — day dividers, attachments that open, drafts that survive and show, Enter-to-send, one delivery vocabulary — and the two apps keep their own objects (a person is not a channel). |
| `c811c9e2df` | Reading: resume, speed, captions, full size, honest staleness; bookmarks ranked in the address bar; a failed subscription stops looking healthy. |
| `2e174974ef` | Files and the ledger: a change you can take back, and nothing Files says expires. |
| `e1685dc4e0`, `ec17be0cb0`, `66cebe0c8f`, `81975d93cc`, `1e53c6b0d8` | The decision half of the developer tools: a merge you can save, a branch switch and a conflict, a query you can stop, a request you can replay, a bell you can see. |
| `dcc5de891d`, `94ecfcd0f8` | The clock answers for the platform, and TOTP asks it instead of shelling out. |
| `b8c9543bc5` | A slideshow dwell is a preference, not motion timing; copy carries its own punctuation instead of a separator glyph. |

## 7.3 The four splits, which the size policy asked for

`3fadc09937` (chat), `a880927811` (photos), `68eadab54c` (video-player), and tube's, in flight.
Each is code motion along a seam that already existed, named in the new module's doc comment.

## 7.4 What is still open

- `SHELL-01`, the blocker: no input method on the lock screen. In flight, and the only finding
  where "not fixed" means some people cannot use the product.
- `T11`: a capture's output depends on what captured before it. Named, not fixed — so `M10`'s gate
  is a diagnostic to run by hand until runs are isolated.
- `SHELL-02..17` (17), and the creation and utility app clusters.
- `T10`: one antialiased pixel still differs between identical runs of the calculator's graph.
- Two policy steps fail on a clean base and are not ours: `capnp` bindings (unavailable in a
  worktree) and one suite test about a fetched bundle.

# Closing: what "finished" means here, and where the head-room ran out

This audit should stop now, and it is worth being precise about why rather than just stopping.

**What was done.** 170 instances with file and line in Volume I; nine causes, nine people and thirteen
moves in Volume II; two apps rendered and driven through width, text-scale, locale, theme and
matrix-vs-viewport comparisons; contrast measured numerically rather than judged; and four of my own
claims retracted because the pixels disagreed with the reading. The most valuable results were not
app defects at all: a resize bug that only appears when a window *becomes* narrow (`T1`), a capture
pipeline that renders in the wrong typeface (`T2`), and a localisation signal that corrupts itself
(`T3`).

**Why stopping is right.** Everything left is a *sprint*: 15 apps onto one breakpoint, ~40 apps onto
the generated command surfaces, 9 apps onto Jobs, 8 onto the ledger. None of it needs more analysis —
it needs the moves to exist and then the mechanical work of adoption. More auditing now would produce
a longer list, not a better product.

**The honest limits of what is here.**
- No latency data at all (`T7`), so nothing in either volume says "slow". The first frame is unmeasured.
- No real assistive technology. Accessibility findings are read from the a11y trees, not from Orca or
  TalkBack.
- Only two apps re-rendered. The other 68 are source-audited; `F2` in Volume I records what happened
  when I trusted old artifacts instead.
- No live backends (that is `live-path-audit.md`'s subject), no touch hardware, no RTL, no theme other
  than dark and one high-contrast pass.
- Nonetheless, the *classes* are settled. A new app written tomorrow will fail this audit in exactly
  the nine ways, and the gate in `M10` would catch it — which is the point.

**The definition of done, for whoever picks this up.** Not "all 170 fixed". It is:

1. `tilo-design-review` passes in CI, with all eight assertions, on every app.
2. A privileged control cannot be drawn without a resolved capability.
3. A number cannot be shown without its measure and its as-of.
4. A second app cannot implement progress, notices, save prompts, zoom, find, breakpoints, edit menus,
   shortcut sheets or fullscreen on its own.
5. Every change of what is shown arrives and leaves, and the test says so.

When those five hold, the audit is obsolete — which is the only good ending an audit can have.
