# How things arrive

Installing software asks a person to make a decision nobody has ever been
equipped to make. A dialog appears. It says this program would like to make
changes to your computer. There is no description of the changes, no bound on
them, and no way to find out afterwards what they were. The only inputs to the
decision are a logo and a feeling. Then you click yes, because you wanted the
program.

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

Some things **act**. Applications, codecs, themes, input methods, the runtime
itself. They change what the machine does.

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
font, an input method, a configuration layer, the OS image. It is signed,
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

That single rule deletes the install-time code-execution class outright, and it
is what makes the rest affordable. If adding cannot hurt you, adding does not
have to be a decision. So it stops looking like one. There is no administrator
password, no progress theatre, no warning. There is a button that says **Add**,
and it means *keep this*. Beside it, one sentence says what the package expects
to be able to do — the only claim on the screen the system will actually hold it
to, and therefore the only one worth reading.

Next to it is a button that says **Open**, and it runs the thing right now:
ephemeral room, no grants, gone when you close it. This is the property the web
has and native platforms gave up — that you can click an unfamiliar thing and
look at it without entering into a relationship with it. Sandboxing is what
earns it back. It also reframes Add honestly: not *do you trust this*, but *do
you want to keep it*.

Removal is total, because there was never anything global to undo. It takes the
software away and leaves what the software made — one directory per room, which
you can keep or delete — and there is nowhere else to look. Nothing lingers in a
lib directory somewhere to haunt the machine.

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
them. The laptop across the table. The mirror on the office network. A CDN. The
publisher. A USB stick. Every one
is verified locally and completely, which means the question *is this source
trustworthy enough to download from* stops existing.

The practical version of that: someone in a shared workspace is using a tool you
do not have, and adding it is a transfer over the connection you already have
open. No round trip to anyone's infrastructure, works on a train. Distribution
becomes a side effect of collaboration.

And because a source only vouches, curation stops being a chokepoint. Anyone may
publish a package, anyone may run a source, anyone may keep a list. A catalogue
*is* a list somebody maintains — the qOS catalogue is a list published by the
qOS org, with no powers your own list of favourites lacks. It is a default, not
a gate.

## Reproducibility is not safety

Nix gets something profound right: a machine that can be described and rebuilt
exactly is a machine you understand. Nothing here disputes that, and the Profile
below is an attempt to keep the property while paying less for it.

But reproducibility answers exactly one of three questions, and not the one that
decides whether you are safe.

**Provenance**: did these bytes come from who they claim? Signatures answer
this, cheaply and well.

**Integrity**: do these bytes correspond to the source they claim? This is
reproducibility's question. qOS treats it as a *check* rather than a property: a
rebuilder is anyone who takes a package's declared inputs, rebuilds, and
publishes a signed attestation that they got the same hash, and the listing
shows the count — *reproduced independently by four rebuilders*. Reproducibility
that nobody verifies is a press release. Putting the number in front of users is
what makes rebuilding worth someone's time.

**Authority**: what can it do once it runs? This is the question every other
system answers last, if at all — which is why sandboxing arrives late and stays
optional. It is also the only one still standing when the first two fail.

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

An application asks for a capability and gets back a handle or *not available*.
Whether that answer came from a preset, a standing grant, a prompt you just saw,
or a policy that will never allow it, is invisible to the application. There is
one code path, and the application is on the far side of it.

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
- You opened a record with the application. That record is granted.
- You pasted. That clipboard item is granted.

No dialog appears, because you already said it. This is capability handoff as
the primary mechanism rather than a fallback, and it eats the overwhelming
majority of what applications ask for.

It works because of a confinement that is not a grant at all: an application
opened in a room is scoped to that room, sees nothing outside it, and starts
empty. Handoff can afford to be granular precisely because the floor underneath
it is already small.

What is left over is short enough to take seriously: camera, microphone,
location, screen capture, background execution, open-ended network, and *all my
records of type X*.

Those get a real moment, with real shapes — **just this time**, *while it is
open*, *until I revoke it*. While any of them is live, the OS shows it: an
indicator drawn by the compositor that no application can suppress or imitate,
which is also the fastest way to turn it off.

Network is on that list. No application reaches the network by default, which
sounds severe and mostly is not, because most applications need exactly one
origin and that origin is implied by something you did — you configured the mail
account, you pasted the feed address, you signed in. What warrants a prompt is
open-ended egress: *this application wants to reach anywhere.* That is rare, it
is worth interrupting for, and it is the sentence people most want to be able to
answer no to. "This application has no network access" becomes a checkbox on
every listing that actually works.

The OS authors can attach a preset to a package — *this map viewer expects
network and location* — and so can an employer, or any attestor. But a preset is
a **proposal**. It is on the listing before you add, and on the screen at the
moment you add, and it takes effect because you added the app while seeing it.
Strip it afterward and the application keeps running. The publisher writes what
their app needs; a curator vouches for it; your presence is what turns it into
authority. The distinction is the whole thing: a preset is a sentence on a
screen you were looking at, and a manifest is a file you have never seen.
Nobody — not the publisher, not a curator, not us — can turn the second into a
grant.

Which is why silent updates are safe. **No update ever widens authority.** A new
version that wants more gets exactly what its predecessor had, and a request at
the moment it first needs the new thing, with you there to answer it. There is
no path by which an application acquires power while nobody is looking.

Two smaller things follow. A package nobody you trust has vouched for does not
get a frightening dialog; it gets a **tighter default** — no network, no
standing capabilities, no background execution — and a plain sentence on the
listing saying so. Warnings train people to click through; defaults do not. And
a denied capability must read to the application as *not available*, identical
to the machine simply not having a camera, and the application has to stay
useful without it. That last part is not politeness. **A permission system is
only real if saying no is cheap.** If refusing costs you the app, you will not
refuse, and the prompt was theatre.

Finally, the application that is not malicious, merely expensive. Phones already
report this — a battery screen that ranks your applications by how much they
cost you — and then do nothing with it, because reporting is not a bound. A qOS
manifest declares memory, CPU and disk ceilings, and the launcher enforces them
as cgroup limits. Consumption then belongs in the same surface as permissions,
because *this application used forty minutes of CPU in the background yesterday*
is the same kind of fact as *this application used your microphone* — and here
it is a fact with a lever next to it.

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
way two people are.

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

What you get for it: your machine has a hash. A fresh install, that hash and
your sources reproduce it — the machine, not your documents, which are records
and live in rooms. History is continuous rather than generational, so you can
revert your terminal without reverting your machine. And *what is different
about this computer* is a diff you can look at, any time.

## Content, finally

An application is a lens, not an owner. That is already true inside a qOS
machine. A room is a context — a workspace, a project, your own desk — and it
holds records by reference rather than owning them. An ebook reader is a way of
looking at book records that happen to be in this room, and two people sharing a
book are sharing one document rather than two copies drifting apart.

Record types are declared by packages, which is what lets one library view show
a book, a playlist and an article side by side. The OS knows nothing about what
an article is and can still display one.

Sharing across machines is that same mechanic reaching past the edge of your own
rooms, and it needs no new vocabulary.

**Someone shares a record by offering you a reference.** It lands in *Shared
with you*, which is a queue of pending references — not a room, not a library,
and not usable until you file it into a room, because a record is only reachable
through a room that references it. Accepting it is the same gesture as pulling
one of your own records in from another room. The property being defended is
that **nothing enters your rooms because someone else decided it should**. A
room contains what you put in it.

Being able to put something in front of you at all requires a relationship — a
shared room, a contact, a subscription. Spam is not filtered here; it has no
address to arrive at. The cost is real and worth saying out loud: first contact
needs an introduction, the way it does in physical life.

A record can also be made **public**, which makes it *reachable* rather than
*findable*. Anyone holding its identity can fetch it, so a reference can be
handed out beyond your contacts — posted, mailed, printed on a poster.
Withdrawing it stops future changes from flowing and does not recall what anyone
already has, which is a fact about giving people data rather than a flaw in the
design, and the interface says so instead of implying otherwise.

Finding public things you have no connection to — indexes, search across
machines, ranking — is deliberately not part of this. It is a large problem with
only weak guarantees available to it, and nothing above depends on solving it.
What you can search is what you can already reach: your rooms, your
subscriptions, your catalogues.

Subscription is the primitive underneath all of it, one concept doing five jobs:
following a person, taking a configuration layer, trusting a source, following a
catalogue, and letting someone reach you. One signed edge with a scope. One
place to see them all, one place to revoke, and portable, because they live in
your Profile.

## What kind of computer this is

Adding software stops being a decision, because adding cannot run code and
cannot acquire power. Trying something stops being a commitment. Where the bytes
came from stops being a security property. Your configuration is something you
can read, diff, carry and hand to a colleague without ever having written it.
Content moves the way collaboration already moves, and nothing reaches you that
you did not invite.

None of that comes from asking people to be careful. It comes from arranging
things so that carelessness is survivable — which is the only security model
that has ever worked on a computer somebody actually uses.

---

*This describes the model. `plan/distribution.md` has the technical design,
including what is built today and what is not — the packaging, sandbox and
image-reproducibility layers exist; source logs, rebuilder attestations, the
Profile and offers do not yet.*
