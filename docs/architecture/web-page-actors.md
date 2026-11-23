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
  script all come through that one action API, decided by the person's policy like
  any other action.

So "two machines in one tab" is not two pages kept in step. It is one page,
seen from two places and acted on through one API. There is no state to
merge, and nothing to diverge.

## Actors

An actor is whoever the action system admitted the invocation for: a person,
an agent, an app, a bot. Browser passes the engine a name for that invoker
with each page action. The engine keeps nothing else about identity and
decides nothing by it; who may act is decided before the action reaches it.

An agent that works from another of the person's devices is one actor like
any other, under its own name. Where it works from is said beside the name,
never inside it. The desktop's MCP server verifies the certificate of the
device a connection comes from. The run that connection works in keeps that
device, and the action system tells Browser the device with every action of
the run. Browser takes the device from there and from nowhere else: not from
what the caller says of itself, and not from the engine, which is passed the
name alone. An actor on the device that runs the page has no device beside
its name. An agent is the same actor in everything it does, and every session
of one MCP client is that one agent: two programs of one client act under one
name, on one device or on two. The device beside the name tells apart the
ones that work from different devices. Nothing in an entry tells apart two
sessions on one device; the record's history does, where each change is
authored by the session that did it.

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
and the record's action API refuses it by name, as a value and in a group, from an
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

**The window knows who an actor is for as long as it needs to.** The engine
knows an actor by its name alone, so the tab's window keeps whom the action
system admitted under each name, and from which device: for the sixty-four
actors it admitted last, and for every actor whose page action is running,
waiting its turn, or answered and not yet written into the record, however
many were admitted after it. So a crowd that asks for a busy tab, most of it
turned away and asking again the moment an action answers, costs the actors
working in it nothing. One name may be several sessions, so an action is set
down as the invoker of the invocation that ran it, not as whoever was
admitted under its name last: the engine's answer to a page action names the
place it set the action down at, and the window knows whom it admitted that
invocation for. What no answer places, a hold and how it ended, is its
actor's as the name was admitted last. While an action admitted under that
name from another place is still to be answered or written, such an entry
may be either session's, and says `device_unknown` rather than name a device
that may be the other's. What the engine sets down under a name the window no
longer remembers is still set down as that actor's, and the entry says
`device_unknown`: where the actor worked from is not known. Such an entry
names no device and is not read as an actor on the device that runs the page.
That is left for how a hold ended that the actor answered, when sixty-four
others were admitted before the engine said so: the one entry the engine sets
down under a name while no action of that actor is in the tab. What the actor
did while the window knew it stays as it was set down, in the record and in
its history, and its next action is admitted again, with its device.

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
question is asked once: the requests of a page that wait on it are answered
together, and one whose challenge comes back only after the answer is
answered the same way, for which the engine keeps that answer until the tab's
next document and gives it to no other question. The
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
  that action), or `device_unknown` where the record says so of that action,
  and where it keeps no such action and the window no longer remembers the
  actor;
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
by the first eight digits of its key where it gives none. One whose device
is not known is shown as that, "ada, device not known", never as an agent on
this device. The record under the forms shows both the same way. A secret is
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

A reviewer on another device sees the tab as its page. While a device shows
the window, the Browser hands its session the page as a display list (the
paint stream), which travels the workspace's live session and is drawn by the
device that shows it: no picture of the window is captured. A window that
protects itself from capture hands over no page. The review read and the
record are what say who set what; the page is what the window shows.

**What the watching device draws.** The page is captured as the engine's
layout: every box, and every line of text in the box the engine laid it out
in. The watching device sets each line in that box with its own text stack,
so a line breaks where the engine broke it whatever face the device has. It
sets the line in the first family of the page's list it has, or in one drawn
with the same measures (a page that asks for Times New Roman is set in
Liberation Serif where that is what is installed, which is the face the
engine itself picks there), and otherwise in its own face of the same class:
serif, sans or fixed-width. The class is the list's generic family or what a
named face is known to be; a page that names no face is set by the engine in
its standard one, a serif. What a list asks for is not always what its
engine set: a machine without the face sets another, and a face the page
brought itself is of a class no name tells. So the sharing device asks the
engine which generic family it sets each list like, measured in the page's
frame in a world the page's scripts do not share, once for each list of a
document and again while the page still loads faces. Where the engine set a
list in another class than it asks for, the list is carried as the faces of
it no name tells the class of, then the class the engine set. A page is not
handed over while the engine has still to say this of a list its text is
set in, so no page arrives in one face to arrive again in another; an
engine that predates the ask, or does not answer within three seconds,
leaves each list as its page computed it. What the engine says of a list
is put on the page as it is handed over, and what it says a field shows
into the capture it already gave, which is converted again: the page is not
captured again for either, so a page's first hand-over costs its engine one
capture. The engine is asked for no capture while it is still to answer,
since behind a capture its answer would wait as long as the capture takes.
The files of a page's own faces are not carried. A line is fitted to its box's
width, drawn narrower rather than run into what follows it. A zoomed page is
laid out larger than its styles say, by a factor its root element gives, and
every length a style gives (a text's size, a border, a radius) is carried at
the size the engine drew it. A line the page draws on text, under it or
through it, is carried as a line.

**A form as its person sees it.** A layout snapshot does not carry what the
engine's own theme paints of a control, nor what a field shows where that is
not what it holds. Both reach the watching device as items it already draws
(a fill, a border, a vector drawing, text), made by the sharing device from
what the snapshot says of each control, so a watching device on an older
build draws them too:

- what a field holds, in the room the field has for it: at its own width,
  from the side the field aligns it to, cut with an ellipsis where it is
  longer than the field;
- a date, a time and a number as the engine words them for whoever reads the
  page ("03/01/2027", not the "2027-03-01" the field holds; "mm/dd/yyyy"
  where none is set), and the words on a button the page gave none. Only the
  engine can say these: the sharing device asks it for what such a field
  shows whenever the field holds something new, and the field is drawn empty
  until it has said. An engine that predates the ask leaves the field drawn
  with what it holds;
- the option a select shows, with the mark that says it opens, and the
  picker mark of a date or a time;
- a checkbox and a radio button as the theme draws them: a grey ring while
  empty, the page's accent and a tick or a dot once chosen;
- a slider's groove and its thumb where the value puts it, and a colour's
  swatch;
- a field, a select and a button the page left unstyled, with the theme's
  hairline and small radius rather than the border their style computes to;
  a fieldset's border, which starts halfway down its legend and is not drawn
  behind it;
- the hint of an empty field, in the hint's grey; a disabled control in the
  dimmed colours the engine computes for it;
- the ring the engine draws around the field being edited, which is how a
  reviewer tells where an agent is working, and any outline a page draws.

**A secret is never carried.** Of a password field, a field that was one (a
page that shows a password on request turns it into plain text), a field
the page says holds a card's number or security code, a password or a
one-time code, and one the page draws as dots, the watching device is sent a
dot for each character and no character: the same fields the tab's record
keeps the content of to itself. The engine is never asked what such a field
shows.

The tile that draws the page on another device names its text, each run
where it is drawn, for a reader who cannot see it; it takes no input.

**Whose page it is.** One Browser process serves every Browser window of a
workspace. Each window has its own engine, its own connection to the
session and its own share: what the session asks of one window, that it is
watched or no longer watched, is that window's alone, and what a window
queues of its page leaves on its own connection and no other. The first
window's connection is made by its launcher and names no tab; each later
window's is made by the session and names the tab the window takes. A
window says that it hands its page over in a message that names no window.
The session takes that offer up for the window the connection speaks for:
the tab it gave the connection, else the tab the window names in what it
asks (the `instance` its asks carry, which every window sends with what it
declares for its bar), else the app's only window. It never takes it for
the app's window in front: with two windows that is a guess, and a page
offered as another window's is shown to whoever watches that one. Until a
window on a launcher's connection has named itself, an app with several
windows has that window's page offered as nobody's. Each frame names its
window too (`instance` on `PaintStream`) and is published only as the page
its connection offered; one that names another window is refused and
published as nobody's. A window whose toolkit predates this names no window
in its frames, which are its connection's page all the same.

**Who is sent it.** The workspace's live session takes any member's ask for
any window's page track. The session publishes the page of a window shared
as its page and of no other, and asks no other window's app for one: a
device that asks for the page of a window nobody shared is sent nothing.

**How large a page may be.** The page handed over is the whole document, not
the part in view. Measured on the engine, in a window of 1280 by 800: a
form of eight questions is a whole frame of 1.8 KB, a form of 240 questions
45 KB, an article of 180 paragraphs 109 KB with ten photographs of 3.1 MB
together (the largest 1.35 MB), a table of 2000 rows 1.7 MB, a table of 5000
rows more than 4 MB. What the theme paints of a form's controls is about
300 bytes of the eight questions. The sizes are of the machine that
measured: every run of text carries its family list, and that machine's
engine sets a page that names no face in its sans, which is a shorter list
than the engine's standard face by name. One message from the app to its session carries less than 1 MiB, so a
payload goes in parts of at most 700 KiB, in order, and the app sends only
as much as its connection has room for, the rest once the session has read
on; the session joins the parts. The live session carries a payload as one object
of at most 4 MiB less the 52 bytes sealing adds, and that is the bound: a
whole frame larger than it reaches nobody. The app does not keep quiet
about such a page: it sends the size and the start of the frame, the
session reads from the first part that the page is too large, and the
sharing device says so in its own words ("The window's page is too large to
send to the devices showing it") instead of waiting. The watching device
has no message for the reason: it says, as of any page that does not come,
that the device running the window was asked and sent none. The app is
asked again every ten seconds, since a page changes. The images of a page
are handed over as the share has room for them, and none twice.

**What it costs.** While a window is watched its page is captured when the
engine painted it anew, at most every 50 ms. A page nothing changes costs
nothing: the engine wakes nobody, no frame is drawn and nothing is handed
over, measured over four seconds watched and four unwatched. A change the
page makes to itself is in what was handed over about 20 ms later. The
changes are taken where the engine's events are taken, so none waits for a
frame to be drawn; a change that comes sooner after a capture than those
50 ms is captured when they have passed, at a wake the view asks for. A
capture is asked for once, and its answer is the page however long the
engine takes over it: a large page on a busy machine takes longer than
three seconds, and asking again would have the engine start over. An engine
that has not answered after three seconds is asked something small behind
the capture, and again each time after twice as long. What an engine is
asked behind a capture it answers behind it, so one still at work answers
the capture first, and one that lost the request answers the small ask
alone and is then asked for the capture again.

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
actions and run it with arguments, and the action API refuses a wrong type or an
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

A Browser window is a window of the device whose engine holds its page, and of
no other. Browser says so of its windows in its manifest (`[entrypoint] window
= "device"`), and the session does the rest for any app that says it: the
device writes the window into the workspace's layout as one that runs on it,
and every other shell lists it as that device's window and starts no Browser
for it ([Workspaces](workspace.md), "Sharing a window"). What Browser keeps in
records (bookmarks, history, the tab's record, continuing a page on another
device) is unchanged: that is the Browser's records, not its window.

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
  desktop's MCP server and works there under a name of its own, as an agent
  on the desktop itself does. Its page actions take their turn in the tab,
  are recorded under that name with the device the MCP server verified it came
  from (see "Actors"), and the person is asked before one that acts on the
  page unless they let that agent go on.

So two agents on two machines in one tab are two connections to one
desktop's MCP server, and a third machine is a third. Nothing is merged.

What checks this: the session's window share tests run three sessions, one
sharing a window and two viewing it. The remote end-to-end run
(`scripts/e2e-remote/run.py`) has an agent on one machine work on another
through its MCP server under its own name, and carries three steps for the tab:
two machines, then three, answer one form in one Browser tab while the
tab's person reads who set each field; and the tab's record and that review
name, beside each agent that came from another machine, the device it came
from, by its whole key and its name, and none beside the tab's own agent.
The device's way to the tab is checked in parts on one machine: the server's
run keeps the device its connection's certificate names, the broker tells an
application that device and never one the caller names, and the Browser's
record, its field review and its review screen keep and show it. The tab
steps also read that the Browser's window stands live on the device that runs
it and that no other machine opened a Browser of its own in its tab. A further
step has one device of the person share the Browser's window by its tab and
another hold its picture in the tab its shell lists the window in, as a window
that runs on the sharing device, before the share, during it and after it
ended.

On three booted machines the three tab steps pass: two machines, then three,
answer one form in one tab, and the record and the review name each remote
agent's device by its key and its name and none beside the tab's own agent.
The share step has not passed there. When it first ran, the sharing device
said it shared the window, the other said it watched it, and no picture
crossed. Two causes were found and are closed in the session: a device that
did not reach the device relaying the workspace's live session when its session
started never asked again, and a shell that listed the window as its own view
of it had no tab to picture the shared window in. The second is held by the
shares' own tests, which carry the workspace's governance between two
sessions by hand.

After the session came to open another device's window as each device's own
view of it, the tab steps broke on the machines: every device started a
Browser of its own for the tab, a second start of a running Browser opened no
window, and the shells' launches overwrote the tab in the layout until the
device that ran the page had no window in it. A Browser window is now written
as a window that runs on its device, which no other device opens. The shell
model's, the session's and the shares' own tests hold that; the machines have
not run it yet.

When it ran again the pictures crossed and then stopped: the device relaying
the live session turned the sharing device away, and went on doing so through a
restart of either session. A relay turns away a device whose keys are behind
its own, and the sharing device's were. The host had been stopped and copied
a second time; back on, it admitted the new copy, which moved the
workspace's keys one epoch on, and it said that change once, to the members
linked to it at that moment, before the first copy's link was up again. A
site that hosts a workspace keeps each channel's newest frame and replays it
to a member that comes back; a device that hosts one kept none. It now hands
each member that links what its channels hold of the workspace, so a member
that was away when the keys moved follows them when it is back, and the
relay's check is as it was. The relay says why it turned a device away, in
its journal and in its own reading of the workspace's live session (`turned_away`),
and the device it turned away says which epoch's keys it presented
(`presented`): the two side by side name a refusal that read alike on both
ends before.

That third cause is held by a test of real sessions in the order the
machines were made and started (`window_share_live`), which fails without
the change and passes with it on the tree the machines ran. On this tree
seven of the eight tests that run real sessions do not reach the share: five
stop where a workspace made on one device should arrive on the person's other
one, and two where a copied machine waits for its host to take it in. The
eighth, the one in the machines' order, reached the share and passed in the
one run made here: the change carries over, and the tree's own tests are not
yet a hold on it. The step has not been run on booted machines since
(see "Not built yet"); it now judges by what the two sessions publish of the
share, when the first picture came and again twenty seconds on.

## The cases

| Case | How it holds |
| --- | --- |
| Two agents, one tab | Turns, own reads, told of the other, one record |
| Two agents, two tabs | Two pages with two records; one profile, so the site's own session (a basket, a sign-in) is shared between them |
| Two devices, one tab | One page on the device that runs it; the other sees the shared window and types into it, and its agent acts through the desktop's MCP server |
| Two agents and a person | The person's typing is direct input; the review read tells the three apart |
| Three devices, one tab | As two; each device's actor has its own read and is told of the others, and each viewer of the shared window has its own picture and seat |

## Limits

How many can work in one tab is bounded in six places. Each says what
happens to the one beyond it.

- **Connections to the desktop's MCP server: sixteen at once.** One count
  holds the login's own programs, the programs inside its environments and
  the agents on the person's other devices together, so sixteen agents, on
  however many machines, is the most one desktop serves at a time. A
  connection holds its place from the moment the MCP server takes it, while the
  person is still asked about it, until it closes. The next connection is
  told that the MCP server is serving sixteen connections, as many as it serves at
  once, that it is closed and nothing it asked ran, and to connect again
  once one of them has closed. It is not queued; one that connects after
  another has closed is served. A runtime that runs without the desktop
  serves sixty-four connections on its own endpoint and tells the next the
  same.

  A program at the MCP server's own socket, or at an environment's, reads the
  refusal as an error that names no request, since none of its requests was
  read. An agent on another device reads it as the server's answer, in place
  of the answer that the server opened: the device that carries it tells its
  client that the other device did not let it in, and why. Connections from
  other devices arrive between the passes of the session that serve them.
  Each takes its place as it arrives and waits in it, so no more of them
  wait than the MCP server has places free.

  Who may connect is decided before the MCP server says it is full. A program of
  another account or inside a sandbox, a program outside the environment
  whose socket it reached, and a device that is not the person's are refused
  as that, whether the server has room or not. The refusal names the bound,
  which is the same for everyone, and nothing of who holds the places.

  Places are also held by what is no agent of this desktop: a client this
  desktop carries to another device's server, for as long as it is carried,
  which holds a place at that server too, and a program inside an app that
  drives the control protocol. Asking whether a device's server answers takes
  none.

  The number is what a connection costs the session. Each keeps two threads
  of the compositor's process for as long as it lasts. The requests of all
  of them, sixteen working for each, are worked on one runtime thread and
  eight blocking threads the connections share, and what they ask of the
  session itself is answered on its loop's pass, between frames. A tab runs
  one page action and holds eight waiting, so sixteen agents already
  outnumber the turns of a tab they share. A runtime without a desktop
  serves more because its process draws nothing. No place is kept for a
  device or a program: sixteen connections from one of them, admitted or
  still waiting for the person's answer, leave none for the others.
- **Requests of one connection: sixteen work at once, sixty-four work or
  wait.** A seventeenth waits for a turn and takes the first one freed, by
  an answer or by its caller's cancel. One past sixty-four is refused at
  once: it is told that it was not run and changed nothing, and to send it
  again once one of the others has answered. Asking how a request stands,
  and cancelling one, take no turn.
- **Page actions in one tab: one runs and eight wait.** One more is turned
  away at once, told what holds the tab and to ask again once the others
  have answered. One that has waited thirty seconds is answered as never
  run. Eight reads of the tab's record are with the engine at once, beside
  these.
- **Actors a window remembers: sixty-four**, beside every actor whose page
  action runs or waits (see "What the tab guarantees").
- **Devices in a workspace's live session: each hears thirty-two others.** A
  shared window, presence and a call travel the workspace's live session, and
  a live session reads the devices in it one by one: three tracks of each, which
  the reading device and the relay hold open for as long as the device is
  there. Thirty-three devices of one workspace therefore all hear each
  other (`MAX_HEARD` in `crates/core/tilo-live/src/live_session.rs`). A device
  beyond that is in the live session and is heard by whoever has room for it. A
  device that has no room for it does not hear it and says so: its live session's
  row counts the devices it does not hear (`unheard`), says why by a word
  (`unheard_why`: `reading-bound`) and in the person's words (`not_heard`),
  and reads them once one it hears has gone. The number is what a further
  device costs, not what the transport allows. Measured with every device
  and the relay in one process, each device a device hears holds about 250
  KiB: 277 MiB for thirty-three devices that all hear each other, a
  gibibyte for sixty-four and 4.3 GiB for a hundred and twenty-eight, and
  the relay is a member's own device. The carrier allows an admitted
  connection the streams thirty-two devices take, 344 bidirectional and
  640 unidirectional (`MAX_BI_STREAMS`, `MAX_UNI_STREAMS` in
  `crates/core/tilo-live/src/native.rs`): twice the tracks read, since a
  track is asked about on a second stream while it is opened, and nothing
  ends an ask that waits for a stream. It allows them on the connection
  itself, once its session is admitted, so a session's endpoint, which is
  bound with the transport of its other protocols, and an endpoint bound
  for the live session alone allow the same. Before that a connection is allowed
  four of each (`PREFACE_BI_STREAMS`, `PREFACE_UNI_STREAMS`) on an endpoint
  bound for the live session, and what its transport allows on a session's. The
  relay admits no fixed number of sessions: who is in the live session is the
  workspace's membership, which has no count of its own (one person's
  devices are 256 at most, `MAX_DEVICES` in
  `crates/core/tilo-peer-state/src/state/personal.rs`).
- **Sessions a live session's relay is admitting: sixteen at once, four of them at
  most of one endpoint, and four more of strangers** (`MAX_ADMITTING`,
  `MAX_ADMITTING_OF_ONE`, `MAX_ADMITTING_STRANGERS` in
  `crates/core/tilo-live/src/relay.rs`). That is what a relay holds for
  devices it has not judged yet, each for no longer than its preface and
  its setup may take. One more is closed at once as busy, with nothing of
  it read and nobody judged: that is no refusal. An endpoint that none of
  the workspaces the relaying session read names as a device waits in the
  four places for strangers, so whoever reaches the relay and says nothing
  holds those and none of the sixteen, and no endpoint holds more than
  four places whatever it opens. Who is admitted is what it was: every
  session is judged by its grant, wherever it waited. The device that was
  told busy asks again after a second, then two, four and eight, and every
  fifteen from then on, each wait up to a quarter shorter by the device's
  own key (`retry_wait` in `crates/session/session/src/live.rs`), so that
  the devices a relay turned back in one moment do not come back in one
  moment, and is taken in once the relay has room. Until then its live session
  reads as told busy by the relay, by name (`relay-busy`), never as
  refused, the relay lists nobody as turned away, and a live session that was
  open before keeps its session while it dials.

What checks this: the runtime's tests reach its endpoint's sixty-four
connections and a connection's sixteen and sixty-four requests through the
endpoint's own accept loop on a socket, and make room as a client does, by
an answer, a cancel and a closed connection. The session's tests reach the
MCP server's sixteen in one session through its own socket, an
environment's socket and the device's endpoint: seven programs of the login,
three inside an environment and six agents on two other devices are served
at once, the next is refused by each way in, in the same words and without
the person being asked, a device of somebody else's and a program outside
the environment are still refused as that, and a connection that closes
makes room for one that came another way. A second test holds the
session's loop still: sixteen connections from other devices are told the
server opened, the next is refused, and the sixteen are served on the pass
that follows. The Browser's tests fill a tab's eight waiting turns and
drive a tab past sixty-four actors, while one of them waits and after one
of them was forgotten, and read the record, its history, who set each field
and the review screen. The live session's tests
stand thirty-three devices in one workspace's live session through a relay their
endpoints reach with four streams until they are admitted, and read that
every one hears every other; with thirty-six, that each hears thirty-two,
counts three as not heard, and hears everyone again once three have gone.
They hold sixteen sessions of a workspace's devices in a relay's admission
and read what the next is told, before and after one of the sixteen is
through, and that devices the workspace does not hold wait in places of
their own and are refused once judged; they open sixteen silent sessions
from endpoints nobody knows and read that sixteen devices of the workspace
are all taken in at once meanwhile; they open sixteen sessions from one
device and read that it holds four places; and they cut the carrier of a
live session whose relay is at its bound and read that it dials until it is
carried again. Two real sessions run the stranger case: one is started
against the relay of the session hosting its workspace while sixteen
endpoints nobody knows hold that relay, and is in the live session at once, never
told busy. The session's own tests read the access, the state and the
words, English and German, of a live session whose relay was busy and of one that
does not hear every device, the latter from a live session with three devices
more than it reads.

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
  server admits in a workspace that confines its agents to runs. A connection
  that works in no run is recorded under the server's own name for it and has
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
- **Acting in a page from another device.** A device that shows a Browser
  window draws its page and sends it nothing: a click or a key in that tile
  reaches no one. Whoever is on another device acts on the page through the
  tab's actions.
- **Another member's agent.** An agent reaches a desktop through its agent
  agent API, which admits the devices of the desktop's own person. A device of
  another member of the workspace sees a shared tab, but its agent has no
  route to the tab's actions.
- **Several machines in one tab: what is left.** Three machines answer one
  form in one tab on the machines the remote end-to-end run boots, and the
  record names their devices. Two things are not shown there. A window
  shared with the workspace has not been seen drawn on the person's other
  device on those machines: when the step last ran, the share's start
  crossed and the watching tab kept the card of a window still arriving,
  while both sessions read as if it were pictured. The session handed that
  tab's tile the page's objects in the live session's framing, which the tile cannot
  read; that is closed, and two sessions on one relay now carry a page from
  the window to the watching tab in a test. The step reads the page's text
  off the watching shell rather than take the sessions' word for it. When it
  ran with that, the page still did not come: the watching shell asked for
  it, the sharing session read that it was asked for, and the Browser was
  never asked. The session wrote its ask for a whole frame to the connection
  that names the window's tab, and the connection of an app's first window,
  which its launcher makes before the app starts, names none; the tests
  handed the session a page with no app on a connection. The ask now goes to
  the connection the app offered its page on, and a test has an app on such
  a connection be asked there and answer there. That a real page then hands
  its whole frame over is held on the engine's side: a suite runs the page
  as the Browser runs it, plays the session's ask and reads the frame out of
  the share's outbox (`apps/browser/chromium/tests/page_share.py`). An app
  that is asked and hands nothing over within ten seconds is asked again,
  and both devices say which side waits: the sharing one that the window's
  app has not handed its page over, the watching one that the device running
  the window was asked for it and sent none. The step has not been run on
  machines since. What a page on its way was found to do wrong since, and
  what is left of it: a page was offered as the page of the app's window in
  front, whichever window spoke; every window of a process shared one
  outbox and one watched flag, so a window's frames could leave on another
  window's connection and be published as that window's, and stopping one
  window's share stopped them all; a page was published to any member's
  shell that asked for it, shared or not; a whole frame or an image over
  700 KiB was dropped in the app without a word, so a table of 2000 rows
  never arrived and an article arrived without its largest photograph; and
  a view drawn from pixels said after its first capture, for as long as its
  window lived, that it had something to repaint. All of these are closed,
  each held by a test (the session's two-window, parts, too-large and
  unshared tests; the toolkit's; the engine suite on pages of real size).
  Left: a picture of more than 4 MiB is not handed over, and its page
  arrives without it, which the device showing the page cannot tell from a
  picture still loading. A page that grows too large while it is watched
  stays on the watching device as it last arrived, and that device is not
  told. A page so large that the engine takes longer than three seconds
  over its capture (a table of 12000 rows) was never handed over, each
  capture given up and asked for again; its capture is now waited for, and
  until it comes the page is said as one its app has not handed over. A
  Browser built before this, with two
  windows in one process, still mixes their frames on its side; the session
  cannot tell them apart, since such frames name no window. Each session says how far its live session
  came, what a share waits for, whether a shared window went out and whether
  the watching tile holds it, and, for the device relaying a live session, which
  devices it turned away and why, which is what the step reads when a page
  does not arrive or does not go on arriving.
  And nothing has run more than three machines. What bounds their number is
  under "Limits". The MCP server's sixteen connections are reached with one
  session and the endpoints of two more devices on one machine, not by
  machines, and nothing keeps a place at the server for each device.
  Thirty-three devices that all hear each other, a
  live session past its bound and a relay at its admission bound have run in
  tests on one machine, never on real ones, and no real session has been
  told busy by a real relay in a test: since endpoints nobody knows no
  longer hold a relay's places, only sixteen of a workspace's own devices
  asking at once fill them, which is sixteen sessions with a shell each.
  The live session's bound of thirty-two devices heard is what a device costs
  today, not what a workspace may hold. Each device read is three
  subscriptions on the reading device and on the relay, so a live session grows
  with the square of its devices; what takes that away is the relay
  carrying every device's presence on one track of its own, and a call's
  audio asked for only while a call runs, and neither is built. Past the
  bound, who a device hears is whoever it came to first, not whom its
  person works with. A relay's memory is bounded by no number of
  sessions: each admitted device reads up to thirty-two. A device of this
  build under a relay of an earlier one is allowed about a hundred streams
  where it asks for up to two hundred, and from about thirty devices on
  the further ones are then neither read nor counted. The relay's reading
  does not say how often it was at its bound, and each admission and each
  change of a workspace's membership reads that membership once for every
  device in the live session. An actor that answers a hold is remembered
  like any other, not held on to until the engine has said how the hold
  ended, so that one entry is where a tab crowded by more than sixty-four
  actors can still say `device_unknown`.
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
