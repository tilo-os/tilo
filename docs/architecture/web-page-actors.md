# Several actors in one web page

A web page in Browser can be worked in by more than one actor at a time: two
agents, an agent on another device, a person typing while an agent fills the
rest, a person reviewing what agents filled. This page says how that holds
together, what each actor is told, and what is not built yet.

The engine's side of it is specified in
[`apps/browser/chromium/README.md`](../../apps/browser/chromium/README.md)
("Several actors in one tab"); this page is the model around it.

## Where a page lives

A page lives in one engine process on one device: its script heap, cookies
and storage never leave it. Everything else is a way of reaching that one
page.

- **Seeing it.** Other devices receive what the page looks like: the live
  picture of its window. Nobody receives a second copy of the page to run.
- **Acting on it.** Every actor acts through the page's published actions on
  `browser.page` (read, click, fill, select, press, submit and the rest). An
  agent on this device, an agent on another of the person's devices and a
  script all come through that one door, decided by the person's policy like
  any other action.

So "two machines in one tab" is not two pages kept in step. It is one page,
seen from two places and acted on through one door. There is no state to
merge, and nothing to diverge.

## Actors

An actor is whoever the action system admitted the invocation for: a person,
an agent, an app, a bot. Browser passes the engine a name for that invoker
with each page action. The engine keeps nothing else about identity and
decides nothing by it; who may act is decided before the action reaches it.

Direct input is not an action. A person typing into the page, at the device
or through a shared window, is the page's own input and is told apart from
every actor's actions.

## What the tab guarantees

**Actions take turns.** A page handles one action at a time. An action that
arrives while another runs waits and runs in the order it came, and each is
answered as its own. It is not refused, so two agents do not have to invent a
retry protocol. The wait is bounded in number and in time, and a caller that
gives up cancels its own request: one still waiting is known to have changed
nothing.

**Each actor reads its own changes.** "What changed since I last looked" is
kept per actor. One agent's read never takes a change away from another.

**An actor is told what the others did.** The page an agent acts on next may
not be the page it read. Every answer to a named actor carries what the
other actors did in the tab since that actor last heard from it, once. An
agent learns the tab moved under it from the answer it was waiting for
anyway.

**The tab keeps a record.** Every action that operated the page is set down
in order: who asked, what they asked for, what it acted on, what it wrote and
how it ended. An element is recorded by its role and name, the description
that still finds it after its document's refs are gone, so the record reads
the same on a later page and a step can be done again from it. A secret is
never in the record, and nor is text the page refused. Reading the page
takes no place in it. The record answers at once whatever the page is doing,
so a reviewer is never queued behind the agents they are watching.

## Reading

Acting on a page starts with reading it, and reading is where an agent's time
and tokens go. Three rules keep it honest and cheap.

- **A paragraph is read whole.** The page's own words are what a reader came
  for. Names and values stay short; prose does not, and text too long for one
  line goes on in the next rather than being cut.
- **A long page is read on, not re-read.** One read carries a bounded part and
  says the line to read on from. Lines keep their numbers while the page does
  not change, so nothing is skipped and nothing is read twice.
- **Words are found by searching.** A search answers with the lines that hold
  the words and their numbers, and a read starts from one of them. Finding an
  element by role and name stays what it was: for controls and headings.

A search reads what the outline reads. What the outline hides, such as the
value of a secret, a search does not find.

## Review

A form several actors filled is reviewed by its fields, not by an account of
the work. The review read lists every field with what it holds **now**, read
from the page itself, and who set that value:

- an actor's action, by actor and the action's place in the record;
- direct input into the page;
- the page's own script;
- nothing, for a field no one set since the page was first read.

A field that no longer holds what was set says so. This is the property a
review needs: an agent's claim that it entered a passport number is not
evidence; the field holding that number, set by that agent's action and
unchanged since, is. Secrets are reviewed by who set them and their length.

Attribution is by time and has two stated limits. An action opens a short
window its own input lands in, so direct input in the moment after an action
is read as that action's. Tracking starts when the document is first read or
operated, so earlier typing carries no attribution.

A reviewer on another device sees the tab as the live picture of its window:
the engine's own pixels, with a password shown as the page shows it and a
window that protects itself from capture blanked. The review read and the
record are what say who set what; the picture is what the page shows.

The page as a display list (the paint stream) carries what each field holds
too: an input's text, a textarea's lines, the option a select shows, and
whether a checkbox or a radio button is chosen, which the engine's own theme
paints and a layout snapshot does not carry. A password travels as dots. The
session does not take that stream yet (see "Not built yet").

## A form as typed actions

A form is what a page asks of whoever fills it in. The forms read turns each
form into that: its questions by name, each of a kind (text, date, one of,
any of, yes or no, secret, file), the answers it offers, which are required,
what each holds now and the buttons that send it. Questions that share a
label are named with their group ("Date of birth: Day"). Radios of one name
are one question; so are checkboxes of one name.

Answering is one action: the answers by name, and whether to send. It
replaces a find, a fill and a check per control, and it reports per answer.
An answer that opens a new question ("Which country?") is said, and the form
is not sent while a required question is empty.

**Fallback is per field, never silent.** What the form action cannot place
is named with the reason and the ref its own action takes: a secret goes
through the secret fill and its consent, a file through the file action, an
editor written by typing through the fill action. Below those are pointer
and key gestures, and below those the picture of the page. The order is
always typed form, then the element's own action, then gestures.

**The second visit.** Reading a page's forms is deterministic and cheap, so
it is done on the live page each time and there is no stored copy to go
stale. What carries over is the names: the same answers, by the same names,
work on the next visit because a name does not depend on a document. A form
has a signature of what it asks and where it sends it. A caller that learned
a form once names it by signature, and a form that has changed is refused
rather than half filled, which sends the caller back to reading it.

## Other devices

Two things carry a tab to other devices, and both are the session's, not the
Browser's.

- **Seeing and typing.** The person shares the tab's window with the
  workspace. Each member who shows it receives the window's live picture,
  the engine's own pixels, and their pointer and keys reach the page
  through a seat of their own while the person allows it. What they type
  is direct input to the page.
- **Acting.** An agent on another of the person's devices connects to the
  desktop's agent door and works there under a name of its own, as an agent
  on the desktop itself does. Its page actions take their turn in the tab,
  are recorded under that name, and the person is asked before one that
  acts on the page unless they let that agent go on.

So two agents on two machines in one tab are two connections to one
desktop's door, and a third machine is a third. Nothing is merged.

## The cases

| Case | How it holds |
| --- | --- |
| Two agents, one tab | Turns, own reads, told of the other, one record |
| Two agents, two tabs | Two pages with two records; one profile, so the site's own session (a basket, a sign-in) is shared between them |
| Two devices, one tab | One page on the device that runs it; the other sees the shared window and types into it, and its agent acts through the desktop's agent door |
| Two agents and a person | The person's typing is direct input; the review read tells the three apart |
| Three devices, one tab | As two; each device's actor has its own read and is told of the others |

## Not built yet

- **The record as a resource.** The record lives with the tab in the engine
  and ends with it. It belongs in a record of Browser's, so History,
  recordings and sync carry it like every other action log
  ([session replay](session-replay.md)).
- **A page's forms published as actions.** The forms read and the answer
  action are page actions with the form as an argument. Each form should
  appear on `browser.page` as its own action with its questions as typed
  parameters, so the palette, scripts and agents list "Apply for a visitor
  visa" beside the page's other actions.
- **Site memory.** What an agent learned about a site that one page does not
  show (the order of a multi-page flow, which answer a site wants where) kept
  as a record keyed by site and form signature, shared in the workspace.
- **Presence on the page.** Where each actor is acting, drawn on the page
  for everyone watching, as agents are shown in other apps.
- **Who types.** Direct input is one source. Input that arrives through a
  shared window should carry the person it came from.
- **The page as a display list for other devices.** Another device sees a tab
  today as the live picture of its window (see "Other devices"). The
  Browser can also describe its page as a display list, which costs the
  sharing device no video encoding and is drawn sharply at any size, but
  the session does not take that stream yet: an app that offers it is
  refused, and nothing asks a window for it.
- **Another member's agent.** An agent reaches a desktop through its agent
  door, which admits the devices of the desktop's own person. A device of
  another member of the workspace sees a shared tab and may type into it,
  but its agent has no door to the tab's actions.
- **Dialogs in another site's frame.** A consent dialog a site shows in a
  frame of another site reads as empty, so an agent cannot answer it. Real
  shops and news sites put their cookie question there.
- **Reader view of old layouts.** An article laid out in a table without an
  article element is refused by the reader view; its text is read in full by
  the ordinary read.

## Checked against the principles

- **One resource/action/view model.** Every actor uses the page's published
  actions. The form action is one more action on the same resource, not a
  second way in.
- **State is CRDT-backed changes.** The page's state is the site's, in the
  engine. What Tilo adds about it (the record) is an action log and is to
  become a resource; until then it is held where the truth is, beside the
  page.
- **The system does what is needed more than once.** Turn-taking, per-actor
  reads and the record are the tab's, so no agent implements locking or
  bookkeeping of its own.
- **The nice properties come by default.** An agent written for one actor
  works unchanged beside others: it is queued, told what moved and recorded
  without asking for any of it.
