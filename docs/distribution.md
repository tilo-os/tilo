# How things arrive

Installing software is the last place in ordinary computing where a person is
asked to make a decision they have no way to make. A dialog appears. It says
this program would like to make changes to your computer. There is no
description of the changes, no bound on them, and no way to find out what they
were afterwards. The only inputs to the decision are a logo and a feeling. Then
you click yes, because you wanted the program.

Everything downstream of that moment is damage control. Stores exist because
someone had to make the decision on your behalf. Reviews and gatekeepers exist
because the store had to justify making it. Signing exists so that at least the
decision is attributable. Sandboxes were bolted on afterward, which is why they
are so often optional. And the whole apparatus is built around a question that
should never have been asked, because the honest answer to *will this program
harm my computer* is: it should not be able to.

qOS starts there. Not "how do we help people decide what to trust", but "how
much can we take off the table before anyone has to decide anything."

## Three things arrive, and they are not alike

Some things **act**. Applications, codecs, themes, rankers, the runtime itself.
They change what the machine does.

Some things **describe**. Your settings, your keybindings, the sources you
follow, your employer's policy. They change what the machine is configured to
do.

Some things are **acted upon**. An article, a book, a spreadsheet, a route, a
song, a message from a friend. They change what is in your world and can do
nothing on their own.

Every system in use today sends two of these down one pipe and has no pipe at
all for the third. A distribution ships a web browser and a desktop wallpaper
with identical ceremony, so the ceremony fits neither. The web ships executing
code with no ceremony whatsoever and calls it a page. App stores sell magazine
issues as applications. Nix models the first two with real rigour and treats the
third as somebody else's department.

The consequence is that the mechanism which is supposed to be a security
boundary is also a content pipe, so it is bad at both, and nobody can answer the
question *what did I actually let onto this machine.*

So the first decision is a line, and it is not the obvious one. The line is not
code versus content. It is **authority versus inertness**.

A **package** changes what the machine does. An application, a codec, a theme, a
font, a configuration layer, a search ranker, the OS image. It is signed,
content-addressed, immutable, and replaced whole.

A **record** changes what is in your world and can act on nothing. An article, a
playlist, an annotation, a file — a file being simply a record whose identity is
a path. It is alive: a CRDT that converges, is never versioned, and is never
installed.

The interesting cases are the ones that pretend to be inert and are not. A PDF
that runs script. A spreadsheet with macros. A font that is really a program for
a hinting virtual machine. A page that is really an application. qOS does not
adjudicate those. It removes the category: **every parser is a package with no
authority** — a one-shot process with no network, no filesystem, no persistence,
bytes in and structure out. Opening a hostile file becomes a question about the
parser's confinement rather than about the file's intentions, and the answer is
identical for every format.

## Adding is not installing

The word "install" comes from an era when software modified your computer. In
qOS it cannot. There is no shared prefix, no global mutable state, no ambient
filesystem — and one rule that does most of the work:

**Adding a package executes no code.** No pre-install hook, no post-install
hook, no trigger, no compilation, no `configure`. Adding is verifying a
signature and unpacking bytes. A program that wants something to happen on first
run does it on first run, inside its sandbox, like everything else.

That single rule deletes the largest remote-code-execution class in every
mainstream package manager, and it is what makes the rest affordable. If adding
cannot hurt you, adding does not have to be a decision. So it stops looking like
one. There is no administrator password, no progress theatre, no warning. There
is a button that says **Add**, and it means *keep this*. The listing next to it
says what the package expects to be able to do, in a sentence — and that is the
only thing on the screen worth reading, because it is the only thing that is
still true after you click.

Next to it is a button that says **Open**, and it runs the thing right now:
ephemeral room, no grants, gone when you close it. This is the property the web
has and native platforms gave up — that you can click an unfamiliar thing and
look at it without entering into a relationship with it. Sandboxing is what
earns it back. It also reframes Add honestly: not *do you trust this*, but *do
you want to keep it*.

Removal is total, because there was never anything global to undo. An
application's writable state is one directory per room, so removing it is
deleting what it made, and nothing is left in a lib directory somewhere to
haunt the machine.

Updates are silent, atomic, and reversible — old content is still content, so
going back to yesterday's version is a lookup rather than a reinstall. Silent
updating is only defensible because of a rule we will come to: an update can
never acquire authority the previous version did not have.

## Where you got it from is not a security property

A source in qOS is a subscription, not a store. It publishes an append-only,
signed log that maps names to package hashes, and it hosts nothing at all.

Two things fall out of the log being append-only and signed. Your machine can
prove that a name resolves to a particular hash, and it can prove that today's
log extends yesterday's with nothing rewritten in between. A name cannot be
quietly re-pointed at different bytes.

Neither proof helps if a source can show one log to you and a different one to
everybody else. So **every qOS machine is a witness**: machines gossip signed
log heads over the same peer-to-peer substrate that already carries workspace
presence. A source that singles you out puts two contradictory heads into
circulation under its own signature, and anyone who sees both catches it. The
audit population is the user base, and it costs nothing because the network was
already there.

Once a name resolves to a hash, the bytes are just bytes and anybody may supply
them. The peer sitting across the table from you, in the room you are both in.
The mirror on the office network. A CDN. The publisher. A USB stick. Every one
is verified locally and completely, which means the question *is this source
trustworthy enough to download from* stops existing.

The practical version of that: someone in a shared workspace is using a tool you
do not have, and adding it is a transfer over the connection you already have
open. No round trip to anyone's infrastructure, works on a train. Distribution
becomes a side effect of collaboration.

And because a source only vouches, curation stops being a chokepoint. Anyone may
publish. A catalogue is a list somebody maintains — the qOS catalogue is a list
published by the qOS org, with no powers your own list of favourites lacks. It
is a default, not a gate.

## Reproducibility is not safety

Nix gets something profound right: a machine that can be described and rebuilt
exactly is a machine you understand. Nothing here disputes that, and the Profile
below is an attempt to keep the property while paying less for it.

But reproducibility answers exactly one question — *did I get what they built* —
and it is quietly the least important of three.

**Provenance**: did these bytes come from who they claim? Signatures answer
this, and it is the smallest question of the three.

**Integrity**: do these bytes correspond to the source they claim? This is
reproducibility's question. qOS treats it as a *check* rather than a property: a
rebuilder is anyone who takes a package's declared inputs, rebuilds, and
publishes a signed attestation that they got the same hash, and the listing
shows the count — *reproduced independently by four rebuilders*. Reproducibility
that nobody verifies is a press release. Putting the number in front of users is
what makes rebuilding worth someone's time.

**Authority**: what can it do once it runs? Nobody else answers this properly,
and it is the only one that matters when the first two fail.

Because they do fail, and not by breaking. Supply-chain attacks succeed
*through* signatures, not around them: a maintainer account taken over, a
malicious transitive dependency, an insider with commit rights. Every byte
signed, every build reproducible, every hash checked, and the attack lands
anyway. A signature makes it attributable. It does not make it survivable.

A perfect, bit-identical, independently verified copy of malware is malware.

So the effort goes into the third question, and the first two get correct,
boring, standard answers.

One thing the first two do buy, once dependency graphs live inside the signed
root instead of being vendored away: your machine can answer *which of my
applications contain this library* instantly and locally, and a revocation can
name the library rather than the publisher. One bad dependency is invalidated
everywhere, at launch, without waiting for a hundred vendors to notice.

## The app asks, the OS answers

The whole permission model comes out of one rule:

**A manifest never grants anything.**

An application asks for a capability and gets a handle or a shrug. Whether that
answer came from a preset, a standing grant, a prompt you just saw, or a policy
that will never allow it, is invisible to the application. There is one code
path and the app is on the wrong side of it.

That invisibility is doing more work than it looks. An application that cannot
detect refusal cannot nag you about it, cannot break spitefully to punish you,
and cannot hold its own functionality hostage until you relent. The dark pattern
requires knowing you said no.

Neither of the two standard models is right. Declaring everything up front —
Android before it grew up, Flatpak today — asks at the moment you have the least
context, about capabilities described in the abstract, when the only alternative
to yes is not having the app. Prompting at every use produces fatigue, and
fatigue produces assent. Choosing between them is choosing which way to lose.

The way out is to notice how few permissions need to be requests at all, because
most are already implied by something you did:

- You picked a file in the OS file dialog. That file is granted.
- You dragged a record onto the window. That record is granted.
- You opened the app in a room. That room's contents are granted.
- You pasted. That clipboard item is granted.

No dialog appears, because you already said it. This is capability handoff as
the primary mechanism rather than a fallback, and it eats the overwhelming
majority of what applications ask for. What remains is short enough to take
seriously: camera, microphone, location, screen capture, background execution,
open-ended network, and *all my records of type X*.

Those get a real moment, with real shapes — **just this time**, *while it is
open*, *until I revoke it*. While any of them is live, the OS shows it: an
indicator drawn by the compositor that no application can suppress or imitate,
which is also the fastest way to turn it off.

Network is on that list. No application reaches the network by default, which
sounds severe and mostly is not, because most applications need exactly one
origin and that origin is implied by something you did — you configured the mail
account, you pasted the feed address, you opened the map. What warrants a prompt
is open-ended egress: *this application wants to reach anywhere.* That is rare,
it is worth interrupting for, and it is the sentence people most want to be able
to answer no to. "This application has no network access" becomes a checkbox on
every listing that actually works.

The OS authors can attach a preset to a package — *this map viewer expects
network and location* — and so can an employer, or any attestor. But a preset is
a **proposal**. It is on the listing before you add, and on the screen at the
moment you add, and it takes effect because you added the app while seeing it.
Strip it afterward and the application keeps running. The publisher writes what
their app needs; a curator vouches for it; your presence is what turns it into
authority. Nobody — not the publisher, not a curator, not us — can turn a
manifest into a grant.

Which is why silent updates are safe. **No update ever widens authority.** A new
version that wants more gets exactly what its predecessor had, and a request at
the moment it first needs the new thing, with you in the room. There is no path
by which an application acquires power while nobody is looking.

Two smaller things follow. A package nobody you trust has vouched for does not
get a frightening dialog; it gets a **tighter default** — no network, no
standing capabilities, no background execution — and a plain sentence on the
listing saying so. Warnings train people to click through; defaults do not. And
a denied capability must read to the application as *not available*, identical
to the machine simply not having a camera, and the application has to stay
useful without it. That last part is not politeness. **A permission system is
only real if saying no is cheap.** If refusing costs you the app, you will not
refuse, and the prompt was theatre.

Finally, the failure mode that nobody models: the application that is not
malicious, merely expensive. Manifests declare memory, CPU and disk ceilings,
enforced as cgroup limits, and consumption shows up in the same place as
permissions — because *this app used forty minutes of CPU in the background
yesterday* is the same kind of fact as *this app used your microphone*, and it
is the one you currently never get to see.

## Your settings are a document

Here is the thing Nix costs you, stated plainly: to get a machine you can
describe and rebuild, you have to write the description, and the boundary
between what is described and what is not gets drawn per-package by whoever
wrote that package's module. It is always incomplete. What falls outside does
not become imperative — it becomes **invisible**. And you give up the interface,
which is a real loss, because sometimes you want to flip a switch and watch what
happens rather than compile a sentence about it.

qOS keeps the property and drops the price by refusing the premise. Reproducible
configuration does not require you to write configuration. It requires the
system to *record* what you did.

Your **Profile** is a document — the same kind of CRDT document as everything
else in the system. It holds your subscriptions, the packages you have added,
the capabilities you have granted, your appearance, input, keybindings, and
every application's settings. Every control in the system edits it. Flip a
switch, a value changes, and the document has a hash. There is no declarative
layer to keep in sync with an imperative one, because the imperative act *is*
the declaration.

And you can open it as text. Not as an export — as an editor.

This works because the document stores the **syntax tree**, not the values.
Keys, values, comments, groupings and blank lines are each addressable nodes
with stable identities, so the text is a rendering of the tree rather than the
tree being a parse of the text. Your comments survive. Your ordering survives.
When you save, the system diffs your buffer against the tree it rendered and
applies the minimal change — one edited line is one operation, which merges
cleanly with a change someone made elsewhere in the same file and cannot clobber
a value you never looked at.

The buffer is live. Change a setting in the interface while the file is open and
the line changes under your cursor. Text and interface are not two
representations to reconcile; they are two editors on one document, exactly the
way two people are two editors on one document.

You only ever edit your own layer. Values inherited from somewhere else render
as dimmed inlays carrying their origin, and typing over one promotes it into
your layer. *What have I changed* and *where did this come from* stop being two
questions.

Layers are packages, which means taking your terminal setup to another machine
is subscribing to a layer, and an employer's policy is a layer with higher
precedence that you can *read* — you can see exactly which of your values it
overrode, rather than discovering it by feel.

The seam that ruins declarative config is drawn once, by the API, for everyone.
An application declares its settings schema and gets a typed accessor.
Everything declared is configuration: portable, typed, validated, diffable,
synced, revertible. Everything else is state — caches, window positions, scroll
offsets, downloaded models — which is local, opaque, and disposable. No
application author decides this, because the toolkit makes the other choice
impossible.

And no application draws its own settings screen. The OS renders it from the
declaration, the way the compositor draws window chrome. Declaring a setting is
writing a field, not building a UI, and every application's settings are
consistent, searchable, translatable, accessible and scriptable without any
application doing work.

What you get for it: your machine has a hash. A fresh install, that hash, and
your sources reproduce it. History is continuous rather than generational, so
you can revert your terminal without reverting your machine. And *what is
different about this computer* is a diff you can look at, any time.

## Content, finally

An application is a lens, not an owner. That is already true inside a qOS
machine: a room holds records by reference, an ebook reader is a way of looking
at book records, and two people sharing a book are sharing one document rather
than two copies drifting apart.

Sharing across machines is the same mechanic reaching past the edge of your own
rooms, with no new vocabulary.

**Someone shares a record by offering you a reference.** It lands in *Shared
with you*, which is a queue of pending references — not a room, not a library,
and not usable until you file it into a room, because a record is only reachable
through a room that references it. Accepting is the same gesture as pulling one
of your own records in from another room: a search result you activate. The
property being defended is that **nothing enters your rooms because someone else
decided it should**. A room contains what you put in it.

Being able to reach you requires a relationship — a shared room, a contact, a
subscription. Anyone else may **publish**, which is pull-only: the thing exists,
it is searchable, and it never arrives. Spam is not filtered here. It is
structurally unable to be addressed to you.

Publishing attaches a small signed **descriptor** to a record: identity, type,
title, summary, language, tags, author, timestamps, content hash. The descriptor
is public; the content itself can still be fetched on demand and can still
require permission. Withdrawal stops future changes from flowing and does not
recall what anyone already has — which is a fact about giving people data, not a
flaw in the design, and the interface says so rather than implying otherwise.

Record types are declared by packages and published, which is what lets one
library view show a book, a playlist and an article side by side. The OS knows
nothing about what an article is and can still display one.

## Finding things

A world of published records with nothing to find them in is worse than no
publishing at all. Search is the part of the web that is genuinely hard to
replace, and pretending otherwise would be the weakest point in the whole
proposition. So it gets a real design, and it turns on one split.

**An index does retrieval. Your device does ranking.**

An index ingests descriptors and answers queries. Anyone can run one; it holds
no content. Because every result is a signed descriptor pointing at
content-addressed bytes, **an index can omit but it cannot forge** — a lying
index is caught the moment you open a result. Omission is not detectable, and
saying otherwise would be dishonest; the answer to omission is that indexes are
cheap to run, cheap to fork, and you subscribe to more than one.

What comes back is a candidate set *with its signals attached*. The signals
travel; the judgement does not. Your machine ranks.

Three kinds of signal. **Match** is ordinary text and structured relevance,
computed by the index, with no politics in it. **The vouching graph** is who
points at this — list inclusions, subscriptions, citations, attestations — all
signed, typed edges, which makes it a better-structured graph than hyperlinks
because every edge says what kind of endorsement it is. It is walked outward
from your own neighbourhood rather than from a global authority, which is what
makes it resistant in practice: minting a thousand publishers is free, getting
one of them subscribed to is not. **Personal** signals are your subscriptions,
your rooms, your history, and they never leave the device.

Because ranking is local, **rankers are packages** — sandboxed, no network,
handed the candidate set and your local graph. Everywhere else, the index and
the ranker are welded together inside one company, and that weld is the actual
moat. Here a better ranker is something you add. And ranking can explain itself,
because you own every input: *first because three lists you follow include it,
and its author has published in this type for four years.*

Curation is a **list**, and a list is just a record type: an ordered set of
references with notes, authored by somebody, and a CRDT so a list can be
curated by several people at once. Lists compose. Every product that would
otherwise be special-cased is a list — an app catalogue, a magazine issue, a
syllabus, a reading list, an entire distribution. Recommendation becomes
subscription, performed by parties with names, who can be dropped.

Subscription is the primitive under all of it. Following a person, taking a
config layer, trusting an index, trusting a source, letting someone reach you —
one signed edge with a scope, one place to see them all, one place to revoke,
and portable, because they live in your Profile.

Publishing is free; being *ranked* is not. A record nobody has vouched for still
matches your queries and still appears, sorted last and marked unvouched. A new
author is findable immediately and ranked the moment one person lists them.

And it is all one surface. Super+K spans records in this room, records in your
other rooms, offers waiting for you, published records from the indexes you
subscribe to, and packages in the catalogues you follow — each row saying where
it came from. There is no separate app store to visit, because there is no
separate kind of finding.

## What kind of computer this is

Adding software stops being a decision, because adding cannot run code and
cannot acquire power. Trying something stops being a commitment. Where the bytes
came from stops being a security property. Your configuration is something you
can read, diff, carry, and hand to a colleague without ever having written it.
Content moves the way collaboration already moves. Nothing reaches you that you
did not invite, and nothing is ranked for you by a party you cannot name and
replace.

None of that comes from asking people to be careful. It comes from arranging
things so that carelessness is survivable — which is the only security model
that has ever worked on a computer someone actually uses.

---

*This describes the model. `plan/distribution.md` has the technical design,
including what is built today and what is not — the packaging, sandbox and
image-reproducibility layers exist; source logs, the Profile, descriptors,
indexes and rankers do not yet.*
