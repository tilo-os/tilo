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

An agent that works from another of the person's devices is one actor like
any other, under its own name. Where it works from is said beside the name,
never inside it. The desktop's agent door verifies the certificate of the
device a connection comes from. The run that connection works in keeps that
device, and the action system tells Browser the device with every action of
the run. Browser takes the device from there and from nowhere else: not from
what the caller says of itself, and not from the engine, which is passed the
name alone. An actor on the device that runs the page has no device beside
its name. So two agents of one device are two actors, and one agent is the
same actor in everything it does.

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
how it ended. An action of an agent that came from another of the person's
devices also says that device, beside who asked: `device`, the device's
key, and `device_name`, what its certificate calls it. An element is
recorded by its role and name, the description that still finds it after
its document's refs are gone, so the record reads the same on a later page
and a step can be done again from it. A secret is
never in the record, and nor is text the page refused. Reading the page
takes no place in it. The record answers at once whatever the page is doing,
so a reviewer is never queued behind the agents they are watching.

**The record is a resource.** It is `browser.page-record`, a record Browser
keeps in the workspace it is open in, beside its browsing history. Each
action is one change of it, added after the tab's others as the action ends
and authored by the actor the action system admitted. Only the tab's window
adds to it, and only what its engine listed. Setting an action down is the
Browser's own writing, not an action of the record: no caller is offered it,
and the record's door refuses it by name, as a value and in a group, from an
agent and from the person alike. So nobody sets down an action the engine
did not list, under their own name or another's. An entry names the device
the action system gave for the actor it is written as, and no other: one
that names another device, or a device where the action system gave none,
is refused. The change that sets an action down carries that device too, so
the record's history says it of each step. History steps through a tab
action by action, a
recording is a stretch of that log ([session replay](session-replay.md)),
and the person's other devices read the same record. Reading the record
answers from the resource, so it answers after the engine started again or
when the engine does not answer; what is running and how many wait still
come from the engine. A reopened window goes on with its tab's record, and
the place an answer names is the place in that record. A private window
keeps none. A tab keeps its newest 1000 actions and the record its 32 most
recently used tabs; what goes beyond leaves the record's state, counted as
dropped, and stays in its history.

**What a person types is in the record.** Direct input is no action, and no
answer carries it: the engine sets it down and says so, and the window
writes it in its place among the actions, as the person's. It is one entry
to a burst of editing one field, which ends when the person stops for a
moment, leaves the field or an action begins. The entry names the field as
an action's entry does and holds what the field then held. An agent's own
keys and clicks stay that agent's action and are not set down twice. Of what
was written into a secret field, typed or by an action, the record keeps
that the field was set and nothing of what was written: a password, a card
number or security code, a one-time code. The engine watches a document from
when it is first read or operated, which the window's own read of a loaded
page's forms does, so what was typed before that is not in the record.

**What was held is in the record.** A window an agent operates holds what
its page is about to send until the person allows it
([hosted Browser](../hosted-browser.md)): a form submission, a request with
data, a download. That is no action either, and the record says it all the
same, so a reviewer reads that a submission waited, whose action reached it
and who let it leave. The engine sets down that something waits (`how` is
`hold` and `action` what waits: `submit`, `request` or `download`), under
the actor whose action was running when the page reached it and under nobody
when the page sent it on its own. The entry names the page, the origin it
would go to and its method there, and of a download the file's name. When
the hold ends the engine sets down how: `allow` when it was let leave, `keep`
when the page was kept as it is, `drop` when nobody answered, `too_many` when
it was turned away at once, `gone` when the page it was held on went away.
Who answered the engine cannot know, so the window says it with its answer. It is the
person when their allow ran again the invocation that reached the hold: that
invocation is its actor's, whom the held entry already names, and only the
person's own answer runs it a second time. It is whoever the action system
admitted `send-held-request` or `keep-held-request` for, the person at the
window when it names nobody. It is nobody when the window dropped the hold
at its own deadline, and when it turned it away at once. The window asks the
person about eight departures at a time: one more is dropped at once, and
the action whose run reached it is told why, so a page that posts in a loop
never has more than those paused and no request waits on questions the
person has not seen. Every hold ends: the engine drops one no app answers at
a deadline of its own, a little after the window's, so nothing a page was
about to send stays paused for good. A hold also ends when the tab shows
another page: what was held has no page to leave from, and a submission
allowed late would leave from a page the person never saw it on, so the
engine drops it and the record names the page it was held on. The action
whose run reached a hold that ended without leaving is not run again when
the person's allow comes after all: it is told that nothing left. An entry
that comes while an action runs takes its
place after that action's own, as typing does. Neither entry carries what
was about to leave: no request's body, no field's value and no secret.

**A page's file chooser is the person's.** A file input a person clicks opens
the system's own picker, and the page is answered with exactly what they
picked there, or told that nothing was chosen. The chooser is no action of
the window: nothing an agent does answers it or reads what was picked, and a
window an agent operates opens no picker at all. An agent gives a page files
by naming them through `fill-page-files`, which checks each. Whose chooser it
is goes by whose click or key the page heard last: the person's own, or an
action's.

**A server's sign-in is asked of the person.** A page behind a server's
password is held by the engine until it is answered, and the window asks in
the system's one password prompt, over the page. The prompt names the origin
that asks, in full; what the server calls itself is shown as the server's own
words and never as who asks, and a password that would cross the network
unprotected is said to. Only the tab's own document and what it loads from
its own origin can ask: a challenge for something a page loads from another
site is cancelled, so no site can raise a prompt that looks like another's.
The same question is a row of the window with two actions, to answer and to
cancel, so a person, a screen reader and an agent answer the one question the
request waits on. The password is a secret throughout: it is in no row, no
answer and no printed command, the tab's record keeps only that one was sent,
and an agent that would give one asks the person first, as for any secret it
gives a page. What a person typed stays in the prompt until it is sent. The
question ends with its page, its tab or its time, and no challenge is ever
left to the engine's own window.

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

A read follows the tree a person sees. A component is read as what it shows:
its own tree where it has one, what the page handed it where the component
shows that, and an element with no box of its own is read through to what it
holds. Only what is not laid out stays out. Finding, searching and the review
read the same tree, and an element inside a component is acted on like any
other. A component that keeps its tree closed cannot be read; it is listed in
a line that says so, never left out silently. Code is read as code, with its
lines kept.

A page is waited for by what it says. A wait may name words the page must
say, and words it must no longer say, such as a spinner's. It ends when a
read of the page says so or when its time is up, and answers which it was,
with the line that says the words. Without words a wait ends when the page
has loaded and stopped changing, which a page that only shows "Loading..."
while a timer runs has done long before what it loads arrives; so a wait for
words never answers that the page settled.

A frame is its own document, of the page's site or of another. The page's
read shows what the frames it shows hold, under the line that lists each,
and a frame is read whole by its number. A frame keeps its number while
frames around it come and go, and an element read in a frame is acted on in
that frame, so a consent question a site shows in a frame of another site
is read and answered like any other. The frame's document is read in the
frame's own process; nothing reaches into it from the page around it.

The reader view reads an article by the text its blocks hold, so an essay
set in a table or run on in one block is an article too. It copies what a
component shows, so a code example in a documentation page reads as its code.

## Review

A form several actors filled is reviewed by its fields, not by an account of
the work. The review read lists every field with what it holds **now**, read
from the page itself, and who set that value:

- an actor's action, by actor and the action's place in the record, and
  by the device the actor worked from when that is not the device that
  runs the page (`device` and `device_name`, as the record keeps them with
  that action);
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

**The review, for the person.** The person at the window reads the same
truth on a screen of their own. `review-page` on `browser.page` shows the
page's review in the page's place: each form with its questions, what each
holds now, and who set it (an agent by its name, a person typing, the page's
own script, or no one), with a mark on a field that no longer holds what was
set, on a required one left empty and on one the page refuses. An agent
that worked from another of the person's devices is shown with that device,
"ada on Studio laptop": by the name the device's certificate gives it, and
by the first eight digits of its key where it gives none. The record under
the forms shows it the same way. A secret is
said as set or not set. Under the forms stands the tab's record, in order,
with who did each action; what a person typed stands there as typing, and
what was written into a secret field as set, not shown. What the window held
stands there in plain words: what waited for the person and whose action
reached it, who let it leave, who kept the page as it is, and what was
dropped, with nobody answering, as one too many or because its page went
away. The Page menu, a key, the palette and an agent all
open it through that one action, and `show-page` closes it. The review keeps
nothing: it is drawn from the forms, fields and record reads, made as the
tab's unnamed actor, so they take no place in the record and tell no agent
anything. It reads again when any page action answered, when the page shows
another document or finished loading, and when the engine says the page
changed a field or a form of itself or a person typed into one; it reads on
no timer. Only with an engine that does not tell page events is the page
read once a second. Fields read from a document the page has left are not
drawn. Choosing a row is `show-page-field`: the
page again, with that field in view, a scroll that takes its turn and its
place in the record like any other action. The review lists the fields of
up to eight frames the tab's actors read or acted in after the page's own,
under their frame, with what each holds and who set it; choosing one brings
it into view in its frame. A frame nobody read or acted in is not listed.

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

**A form on a page built of components asks what a person sees.** A form's
questions are the fields drawn in it, found along the tree a read follows: a
field inside a component, a field the page hands a form component, and a
form that itself stands inside a component are asked like any other, each
named by the label its component draws. A component that is a field itself
and draws no input of its own is a yes-or-no question when it says it is a
checkbox or a switch; otherwise it is listed as out of reach, with the
reason, and an answer for it is refused by name, never placed somewhere
else. A button a component draws is pressed, and the answer says whether the
form then sent, whether the page took the sending for itself, or that
nothing was sent. A page without components is read as it always was, so a
form remembered from an earlier visit keeps its signature.

**Fallback is per field, never silent.** What the form action cannot place
is named with the reason and the ref its own action takes: a secret goes
through the secret fill and its consent, a file through the file action, an
editor written by typing through the fill action. Below those are pointer
and key gestures, and below those the picture of the page. The order is
always typed form, then the element's own action, then gestures.

**A form is an action of the page.** Each form a page is known to ask is an
action of `browser.page` named after it ("Apply for a visitor visa"), with
one typed parameter per question: text, a number, a date, a tick, or a
choice held to the answers the form offers, and required where the form
requires. A secret or a file is no parameter; the action says which action
takes it. The palette, a script and an agent list it beside the page's other
actions and run it with arguments, and the door refuses a wrong type or an
answer the form does not offer before anything reaches the page. Sending a
form is a second action of its own, so answering never sends: an agent
fills, a person reviews, and sending is decided apart.

**The second visit.** What reading a site's forms found is remembered in the
Browser's document, by site and signature. A page of a remembered site
publishes its forms' actions at once, before anyone reads it. The action
names its form by signature and the engine answers only a form that still
has it, so a form that changed is refused with `form_changed` and nothing
is placed. The window then reads the page's forms itself, as the tab's
unnamed actor, and that read replaces the actions and the memory. What is
remembered is a form's shape, never what anyone answered.

## Other devices

Two things carry a tab to other devices, and both are the session's, not the
Browser's.

- **Seeing and typing.** The person shares the tab's window with the
  workspace (the session's `share`, which takes the window's tab and
  finds the window itself). Each member who shows it receives the
  window's live picture, the engine's own pixels, and their pointer and
  keys reach the page through a seat of their own while the person
  allows it. What they type is direct input to the page. Several members
  view one window at once, each a viewer of its own: one who stops
  viewing ends their own picture, seat and reach, and nobody else's.
- **Acting.** An agent on another of the person's devices connects to the
  desktop's agent door and works there under a name of its own, as an agent
  on the desktop itself does. Its page actions take their turn in the tab,
  are recorded under that name with the device the door verified it came
  from (see "Actors"), and the person is asked before one that acts on the
  page unless they let that agent go on.

So two agents on two machines in one tab are two connections to one
desktop's door, and a third machine is a third. Nothing is merged.

What checks this: the session's window share tests run three sessions, one
sharing a window and two viewing it. The remote end-to-end run
(`scripts/e2e-remote/run.py`) has an agent on one machine work on another
through its door under its own name, and carries three steps for the tab:
two machines, then three, answer one form in one Browser tab while the
tab's person reads who set each field; and the tab's record and that review
name, beside each agent that came from another machine, the device it came
from, by its whole key and its name, and none beside the tab's own agent.
The device's way to the tab is checked in parts on one machine: the door's
run keeps the device its connection's certificate names, the broker tells an
application that device and never one the caller names, and the Browser's
record, its field review and its review screen keep and show it. A further
step has one device of the person share the Browser's window by its tab and
another hold its picture in a tab of its own, as a window that runs on the
sharing device, whatever its desk lists as its own view of that window.

On three booted machines the three tab steps pass: two machines, then three,
answer one form in one tab, and the record and the review name each remote
agent's device by its key and its name and none beside the tab's own agent.
The share step has not passed there. When it last ran, the sharing device
said it shared the window, the other said it watched it, and no picture
crossed. Two causes were found, and both are closed in the session and held
by tests that run two real sessions (`window_share_live`): a device that did
not reach the device relaying the workspace's live plane when its session
started never asked again, and a desk that listed the window as its own view
of it had no tab to picture the shared window in. The step has not been run
on booted machines since (see "Not built yet").

## The cases

| Case | How it holds |
| --- | --- |
| Two agents, one tab | Turns, own reads, told of the other, one record |
| Two agents, two tabs | Two pages with two records; one profile, so the site's own session (a basket, a sign-in) is shared between them |
| Two devices, one tab | One page on the device that runs it; the other sees the shared window and types into it, and its agent acts through the desktop's agent door |
| Two agents and a person | The person's typing is direct input; the review read tells the three apart |
| Three devices, one tab | As two; each device's actor has its own read and is told of the others, and each viewer of the shared window has its own picture and seat |

## Not built yet

- **A page's forms as actions: what is left.** The window reads its page's
  forms itself, as the tab's unnamed actor: once the page has loaded, when
  it moves to another address, when a form action's answer says the form
  became another form, offers other answers or is no longer on the page, and
  when the engine says a form of the page appeared, changed or went. So a
  first visit, a form that grew and a form the page's own script draws later
  have their actions without an agent's read. With an engine that does not
  tell page events, a form drawn later becomes an action only at the
  window's next read or when an actor reads the page's forms. On a page that
  holds a sign-in the window's read publishes each question without the
  answers it offers, until an actor the person admitted reads the page. A
  form in a frame is answered as one typed action through `fill-page-form`
  with its frame, but is no action of the page named after it: the page's
  forms read lists neither the frames a page holds nor their forms, so the
  window cannot find a frame's form without reading the page's outline, and
  nothing is remembered of a frame's forms. Of several forms that ask
  exactly the same the action answers the first.
- **Site memory: what is left.** What the Browser remembers of a site's
  forms is listed by typed actions and shown in the site panel, where one
  form or the site's forms are forgotten; clearing a site's data forgets its
  forms, and clearing all site data forgets every form. No screen lists
  every site, a forgotten site is remembered again on its next visit, and
  clearing data on exit does not forget forms. Memory is the workspace's
  document, so visiting a page that asks a form writes that site and path
  there, except in a private window or while history is paused. It knows
  nothing a form does not show: which answer a site wants where is still the
  agent's to learn.
- **The record across devices.** A window refuses what a caller sends; a
  change to the record that arrives from another of the person's devices is
  merged as that device wrote it, and is not checked against that device's
  engine.
- **Devices in the record: what is left.** The device is known for an agent
  whose connection works in a run of its own, which is every connection the
  door admits in a workspace that confines its agents to runs. A connection
  that works in no run is recorded under the door's own name for it and has
  no device beside it. A private window keeps no record, so its review
  names no device. The name kept is the one the device had when it acted: a
  device renamed later is still found by its key. Direct input through a
  shared window names no device (see "Who types").
- **Holds in the record: what is left.** When the person denies the action
  whose run reached a hold, the action system ends that invocation and the
  window is not told: the hold is set down as dropped once the window's own
  deadline passes, not as kept when they said no. A held download the person
  cancels in the downloads list, instead of answering it, has no entry for
  how it ended, and a download is not ended by its page going away: the offer
is the engine's. A tab that closes with something held sets nothing more
down.
  The record does not tell a departure the page sent on its own from one the
  person at the window started: both are under nobody's name.
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
- **Several machines in one tab: what is left.** Three machines answer one
  form in one tab on the machines the remote end-to-end run boots, and the
  record names their devices. Two things are not shown there. A window
  shared with the workspace has not been pictured on the person's other
  device on those machines: what kept its pictures from crossing when the
  step last ran is closed in the session, and the step has not been run on
  machines since. Each session says how far its live plane came and what a
  share waits for, which is what the step reads when no picture arrives.
  And nothing has run more than three machines. The door
  serves sixteen connections at once and refuses the next as busy; a tab
  keeps track of sixty-four actors and forgets the oldest beyond that
  without saying so, which no test reaches.
- **Gestures in a frame: what is left.** Hover, drag and a drawn path on an
  element of a frame run in the frame's document, a point over a frame is
  answered with the frame's own element, and a find and a search look in the
  frames a page shows: on their own when the page has no match, beside the
  page when asked, saying the frame of what they found. Both ends of a drag
  are elements of one frame; a drag from a frame to the page, or to a point,
  is refused. The Browser's tests check that these requests reach the engine
  and its answers reach the caller; the gestures themselves and choosing
  files in a frame are checked in the engine's tests, not through the
  Browser.
- **Paragraphs in the reader view of old layouts.** An essay set in a table
  or run on in one block is read as an article. Its paragraphs, parted by
  line breaks, are shown with those breaks rather than as paragraphs.

## Checked against the principles

- **One resource/action/view model.** Every actor uses the page's published
  actions. The record offers no action that writes it: actors read it
  through the page's action, and the Browser alone sets actions down. The
  form action is one more action on the same resource, not a second way in.
- **State is CRDT-backed changes.** The page's state is the site's, in the
  engine. What Tilo adds about it is resources: the record, each action a
  recorded change by its actor that only the window writes, and the shape
  of the forms a site asks, in
  the Browser's document. The window holds only where its engine's own count
  stands against the record.
- **The system does what is needed more than once.** Turn-taking, per-actor
  reads and the record are the tab's, so no agent implements locking or
  bookkeeping of its own.
- **The nice properties come by default.** An agent written for one actor
  works unchanged beside others: it is queued, told what moved and recorded
  without asking for any of it.
