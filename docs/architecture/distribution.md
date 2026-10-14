# Distribution — how software, settings and content reach a machine

Three kinds of thing arrive on a computer, and they are not alike.

Some **act**: applications, codecs, themes, input methods, the runtime, the OS
image.
Some **describe**: your settings, your keybindings, the sources you trust, an
employer's policy. Some are **acted upon**: an article, a book, a spreadsheet, a
route, a playlist.

Every system in use today pushes two of these down one pipe and has no pipe at
all for the third. A distro ships a browser and a wallpaper through the same
mechanism with the same ceremony. The web ships executable code with no ceremony
whatsoever and calls it a page. Nix models the first two beautifully and treats
the third as somebody else's problem. The result is that the mechanism which is
supposed to be a security boundary is also a content pipe, so it is bad at both
jobs, and the question "what did I actually let onto this machine" has no
answer.

This document defines the three pipes, the single trust model underneath them,
and the user-facing verbs.

It builds directly on `docs/architecture/rooms.md` (rooms, records, references) and
`docs/architecture/security.md` (the workspace as the security boundary). Where it extends
them it says so; where it answers a question they left open it says that too.

## What is wrong today

Seven failures, each of which some existing system exhibits in its pure form.

**Nobody knows what a package is.** Fonts, wallpapers, browsers and kernel
modules travel through one pipe with identical ceremony, so the ceremony is
calibrated for none of them. Meanwhile a web app — code, executing, with
network access — arrives with no ceremony at all.

**Installation is a privileged act with no bound.** `postinstall` scripts,
shared prefixes, mutable global state, package managers that run as root by
construction. Because installing *is* trusting, every install is a decision;
because every install is a decision, the UX gets heavy; because the UX is heavy,
we invented curated stores to make the decision for us; and now shipping
software requires somebody's permission.

**Distribution conflates hosting, naming and vouching.** Whoever serves the
bytes decides what exists, what it is called, and whether it is any good. That
is why "where you got it from" is treated as a security property, which is an
admission of defeat: it means the bytes cannot speak for themselves.

**Reproducibility is not safety.** A reproducible build answers *did I get what
they built*. It never answers *should they have had that power*. A perfect,
bit-identical, independently verified copy of malware is malware. Worse, the
property usually goes unchecked — reproducibility exists in the abstract while
nobody actually rebuilds.

**Declarative configuration is all-or-nothing.** The seam between what is
declared and what is not gets drawn per-module by whoever wrote that module, and
it is always incomplete. Anything outside the schema does not become imperative,
it becomes *invisible*. And the price of admission is giving up the interface —
you can no longer flip a switch and watch what happens, which is a real thing to
want and not a beginner's crutch.

**Permissions are asked at the wrong time, in the wrong words, and refusing
breaks the app.** Install-time manifests ask when the user has the least context
and no way to say no except by walking away. Use-time prompting produces
fatigue, and fatigue produces assent. And a denied capability that crashes a
feature teaches everyone, permanently, to say yes.

**Nothing bounds an application over time.** The app that is fine today and
compromised at the next update. The app that is not malicious at all, merely
expensive, and quietly eats a battery for a year.

## The model

### 1. Packages carry authority; records do not

The distinction is not code versus content. It is **authority versus
inertness**.

> A **package** changes what the machine does.
> A **record** changes what is in your world, and can act on nothing.

An application is a package. So is a codec, a theme, a font, an input method, a
configuration layer, the runtime bundle, and the OS image — all of them change
the machine's behaviour, so all of them are signed, content-addressed,
versioned, and replaced wholesale. An article is a record. So is a book, a
playlist, a route, a spreadsheet and a file; `docs/architecture/rooms.md` already establishes
that a file is simply a record whose identity is a path.

Packages and records get different substrates because they have opposite
natures. A package is immutable, pinned by hash, and you must not be given
different bytes than anyone else. A record is alive — a CRDT that converges,
never versioned, never installed, pinned by *identity* rather than by content.

The gray zone is where every other system bleeds. A PDF that executes script, a
spreadsheet with macros, a font that is really a program for a hinting VM, a
page that is really an application. Tilo does not adjudicate the gray zone; it
removes it. **Every parser is a package with no authority.** A format provider
is a one-shot process with no network, no filesystem, no service sockets and no
persistence: bytes in, structure out. Handling a hostile file is then a question
about the *provider's* confinement, not about the file's intentions, and the
answer is the same for every file type.

The verbs follow from the split:

| | package | record |
|---|---|---|
| identity | content hash | stable record id |
| lifecycle | added, updated, removed | created, edited, converges |
| where it lives | the local store | a room, by reference |
| how it travels | source log resolves a name to a hash | reference, offer, or publication |
| what it grants | nothing, ever | nothing, ever |

### 2. Adding, not installing

"Install" is a word from an era when software *modified your computer*. A tapp
cannot. There is no shared prefix, no global mutable state, no ambient
filesystem, and — the load-bearing rule —

> **Installation executes no code.**

Adding a package is verifying a signature and unpacking bytes into a
content-addressed store. There is no pre-install hook, no post-install hook, no
trigger, no compilation step, no `configure`. A package that wants something to
happen at first run does it at first run, inside its sandbox, like everything
else. This one rule eliminates the largest install-time remote-code-execution
class in every mainstream package manager, and it is what makes the rest of the
UX affordable: if adding cannot hurt you, adding does not need to be a decision.

So the verb is **Add**, and it behaves like one:

- **Adding grants nothing by itself.** Not filesystem access, not network, not
  hardware. A freshly added application can reach its own payload, the fixed
  system read profile, and one writable directory that is created for it. The
  single exception is a preset a curator proposed and you saw on the way in
  (§5) — which is not the package granting itself anything, and which you can
  strip without the application noticing.
- **Removal is total and instant.** No orphaned files, no leftover units, no
  half-configured state, because there was never anything global to configure.
- **Running without adding is a first-class verb.** Open runs it now — ephemeral
  room, zero grants, discarded on close. Add keeps it. A catalogue listing
  therefore offers both, and Add means "keep this", not "commit to this".
- **Updates are atomic and reversible.** Old content is still content, so
  reverting to yesterday's version is a lookup, not a reinstall.
- **An update can never widen authority.** See §5. This is the property that
  makes silent updating safe enough to be the default.

Per-room data confinement (`docs/architecture/rooms.md`) is what makes removal honest: an
app's writable directory is `<data home>/tilo-rooms/<room>/<package>`, so there
is exactly one place per room to delete, and no application anywhere else on
disk.

### 3. Sources name; they do not host

A **source** is a subscription, not a store. It publishes an **append-only,
signed log** mapping names to package identities. It holds no bytes.

```
source log entry:  name  →  package id (hash of the signed root)
                   + publisher key, timestamp, supersedes
signed head:       log size + root of the entry Merkle tree, signed by the source
```

Two proofs come off that structure, and they are what make a source safe to
trust without trusting it:

- **Inclusion** — this name resolves to this hash, and here is the path proving
  the entry is in the log whose head I already had.
- **Consistency** — the log I see today extends the log I saw yesterday, with
  nothing rewritten in between. Names cannot be silently re-pointed.

Neither proof helps if the source shows different logs to different people. So:

> **Every Tilo machine is a witness.**

Machines gossip signed log heads over the collaboration substrate that is
already there for rooms — the same Iroh gossip that carries workspace
presence. A source that presents one head to you and another to everyone else
produces two heads in circulation, signed by the same key, and is caught by
anyone who sees both. This closes the *compromised registry owner* hole
directly: a malicious source can refuse to serve, and it can publish something
bad in public, but it cannot single you out.

**Bytes come from anywhere.** Because a name resolves to a hash, the bytes are
just bytes, and whoever has them can give them to you: a peer in the room you
are sitting in, the mirror on your office LAN, a CDN, the publisher, a USB
stick. Verification is local and total.

> Where you got it from stops being a security property.

That has an immediate practical consequence: adding an app that a colleague has
open in a shared room is a peer-to-peer transfer over the connection you already
have, offline-capable, no round trip to anyone's infrastructure. Distribution
becomes a side effect of collaboration.

**Curation is not hosting.** A source vouches; a catalogue recommends; neither
serves. A catalogue is a **list** — a record holding an ordered set of
references with notes, authored by a key, and a CRDT so several people can
curate one together — and it has no powers a person's hand-written list of
favourites lacks. The Tilo catalogue is a list published by the Tilo org. It is a
default, not a gate.

### 4. Trust is three separate questions

Systems fail here by answering one question loudly and implying it covers the
others.

**Provenance — did these bytes come from who they claim?** Ed25519 publisher
identity over the signed root; attestor co-signatures with scopes; revocation
checked at add *and* at every launch, because a revocation can arrive between
the two. This is the question signatures answer, and it is the smallest of the
three.

**Integrity — do these bytes correspond to the source they claim?**
Reproducible builds, but as a *check* rather than a property. A **rebuilder** is
anyone who takes a package's declared source and build inputs, rebuilds, and
publishes a signed attestation that they got the same hash. The catalogue shows
the count — *reproduced independently by 4 rebuilders* — and a policy can demand
k of n. Reproducibility that nobody verifies is a press release; the count is
the thing that makes it real, and putting it in the listing is what makes
rebuilding worth doing.

**Authority — what can it do once it runs?** §5.

The relationship between them is the point:

> Provenance and integrity are about trusting the publisher.
> Authority is about not having to.

Supply-chain attacks keep succeeding *despite* signatures, because the
compromise is upstream of the signature — a taken-over maintainer account, a
malicious transitive dependency, an insider. A signature makes such an attack
attributable. It does not make it survivable. Only confinement does. So the
first two questions get correct, cheap, standard answers, and the design spends
its actual effort on the third.

Two things follow from putting the full dependency graph inside the signed root
rather than vendoring it away:

- The machine can answer *which of my applications contain this library* as a
  local query, instantly, without a distribution-wide rebuild.
- Revocation can be **scoped to a dependency**, not only to a publisher. "This
  version of this library is compromised" invalidates every package that
  contains it, at launch, everywhere, without waiting for each publisher to
  respond.

### 5. Authority: the app asks, the OS answers

One rule generates the whole permission model:

> **A manifest never grants anything.** An application asks; the OS answers; the
> application cannot tell where the answer came from.

There is exactly one code path. The app calls for a capability — a camera, a
directory, an origin — and gets back a handle or `NotAvailable`. Whether that
answer came from a preset, from a standing grant, from a prompt the user just
saw, or from a policy that will never allow it, is invisible to the app. This is
what kills the coercion pattern: an application cannot detect refusal, so it
cannot nag, cannot degrade spitefully, and cannot hold itself hostage.

**Presets are proposals, not grants.** A curator — the OS authors, an employer,
any attestor — may attach a preset to a package: *this map viewer expects
network and location*. The preset appears on the listing, before you add, and at
the moment you add. It takes effect because **you added the package while
seeing it**, not because it was written down. You can strip it immediately and
the app keeps running. The distinction matters because it is what stops the
manifest from being a self-service authority form: the publisher writes what the
app needs, a curator vouches for it, and the user's presence is what turns it
into authority.

**No update ever widens authority.** A new version that asks for more gets the
answer its predecessor had, plus a request at the moment it first needs the new
thing — with the user present. There is no path by which authority is acquired
while nobody is looking. Not for a publisher, not for a curator, not for the OS
authors.

**Most permissions should not exist as prompts.** The overwhelming majority of
what an application needs is already implied by something the user did:

| the act | the grant |
|---|---|
| picking a file in the OS file dialog | that file |
| dragging a record onto the window | that record |
| referencing a record into the room | that record, in this room |
| opening the app in a room | that room's references |
| pasting | that one clipboard item |

None of these show a dialog, because the user already said it. This is
capability handoff as the *primary* mechanism rather than a fallback, and it is
what makes the remaining prompts rare enough to be worth reading.

**Standing capabilities are the short list.** Camera, microphone, location,
screen capture, background execution, open-ended network, and "all my records of
type X". These get a real moment, with grant shapes — **just this time**, *while
open*, *until I revoke* — and while any of them is live the OS shows it: an
indicator in the chrome, drawn by the compositor, that no application can
suppress or imitate, and that is also the affordance for revoking on the spot.

**Network is a capability.** No application reaches the network by default. But
most only need one origin, and the origin is usually implied by an act the user
already took — you configured the mail account, you pasted the feed URL, you
opened the map. Those need no prompt. What needs a prompt is open-ended egress,
*this application wants to reach anywhere*, which is rare, genuinely worth
interrupting for, and is the sentence users most want to be able to answer no
to. "This application has no network access" is a checkbox, on every listing,
that actually works.

**Refusal must be survivable.** A denied capability surfaces as
`NotAvailable`, identical to the capability not existing on this machine; the
runtime SDK already returns `Result<T, NotAvailable>` on every service call and
never panics. An application must remain useful without it.

> A permission system is only real if saying no is cheap.

If refusing costs the user the app, they will not refuse, and the prompt was
theatre.

**Untrusted is a preset, not a warning.** A package with no attestation from
anyone you trust does not get a scary dialog; it gets a tighter default — no
network, no standing capabilities, no background execution, handoff grants only
— and a plain line on the listing saying so. The user can loosen it. Warnings
train people to click through; defaults do not.

**Budgets bound the merely expensive.** A manifest declares memory, CPU-share
and disk ceilings; the launcher enforces them as cgroup limits. Consumption
appears in the same surface as permissions, because "this app used 40 minutes of
CPU in the background yesterday" is the same kind of fact as "this app used your
microphone", and it is the one nobody currently gets to see.

### 6. The Profile: settings are a document you can also read

`docs/architecture/rooms.md` names two categories of state and gives one of them a home:
content records are room-scoped, while *person and device settings* — identity,
appearance, input, credentials — follow the person or the machine. This is where
those live.

The **Profile** is a CRDT document describing a machine and the person using it:
subscriptions, packages added, capabilities granted, appearance, input,
keybindings, and every application's settings. It is the same substrate as every
other document in the system, which means it syncs, merges, has history, and can
be shared live.

**Every control edits the Profile.** There is no imperative layer to reconcile
with a declarative one, because the imperative act *is* the declaration. You
flip a switch; a value in a document changes; the document has a hash. You never
have to write configuration to get reproducibility.

**And you can edit it as text.** This is a design requirement, not an export
feature, and it takes four things to be true:

1. **The document stores the syntax tree, not the values.** Keys, values,
   comments, grouping and blank lines are each addressable CRDT nodes with
   stable ids. The text is a *rendering* of the tree; the tree is not a parse of
   the text. Comments and ordering survive round-trips because they are in the
   document, not decoration around it.
2. **Saving applies a structural diff, never a replacement.** Parse the buffer,
   diff against the tree it was rendered from, apply the minimal ops. One
   changed line is one op. It merges with someone else's simultaneous edit
   elsewhere in the file, and it cannot clobber a value you never looked at.
   (This is the same diff-and-splice discipline that keeps Automerge text from
   accumulating tombstones — writing the whole document on every save is the bug
   we already know about.)
3. **The buffer is live.** Change a setting in the interface while the file is
   open and the line changes under the cursor. Text and interface are not two
   representations to reconcile; they are two editors on one document, exactly
   the way two people are.
4. **You only ever edit your own layer.** Inherited values render as dimmed
   inlays carrying their origin; typing over one promotes it into your layer.
   "What have I changed" and "where did this come from" become the same view.

**Layers are packages.** A layer is a fragment of Profile published like any
other package — signed, content-addressed, from a source, subscribed to. Taking
your terminal setup to another machine is subscribing to a layer. An employer's
policy is a layer with higher precedence, and — because it is a document, not an
opaque push — you can see exactly which of your values it overrode and why. Your
own edits are the topmost layer and win over everything advisory.

**The declared/undeclared seam is drawn by the API, once, for everyone.** An
application declares its settings schema in its manifest and gets a typed
accessor; everything declared is configuration and lives in the Profile —
portable, typed, validated, diffable, synced, rolled back. Everything else is
state: caches, window positions, scroll offsets, downloaded models. It is local,
opaque, disposable, and never syncs. Nobody decides this per application,
because the toolkit makes the other choice impossible.

**No application draws its own settings screen.** The OS renders the settings
surface from the declaration, the way the compositor draws window chrome and
owns Ctrl+K. That is what makes every application's settings automatically
searchable, diffable, scriptable, translatable, accessible and consistent — and
it means declaring a setting is writing a field, not building a UI.

What falls out: the machine has a **hash**; a fresh install plus that hash plus
your sources reproduces it, minus your records. History is continuous rather
than generational, so rollback is **per-subtree** — revert your terminal, not
your machine. And "what is different about this computer" is a diff against
defaults, on screen, at any time.

Applications that do not cooperate — a foreign Flatpak, a legacy binary — get
their config directory versioned as an opaque blob. Degraded, and honest about
being degraded.

### 7. Records leave the machine

`docs/architecture/rooms.md` settles the mechanics inside a machine: a record exists once,
rooms hold **references** rather than copies, and the reference is what grants
access. Sharing and publishing are that same mechanic, extended past the edge of
one person's rooms.

**An offer is a proposed reference.** Someone shares a record with you by
offering a reference — the record id, plus the capability to reach it, plus who
sent it. It arrives in *Shared with you*, which is a queue of pending
references. It is not a room, it is not a library, and the record is not usable
until you file it into a room, because a record is only reachable through a
room that references it. Accepting is the same gesture as bringing your own
record in from another room: it is a search result you activate.

The property being protected is worth stating: **nothing enters your rooms
because someone else decided it should.** A room contains what you put in it.

**Reach requires a relationship.** Offering a record to a specific person
requires a shared room, a contact, or a subscription. Nobody else can put
anything in front of you. Spam is not filtered here; it has no address to arrive
at.

**A record can be made public, which makes it reachable, not findable.** A
public record is fetchable by anyone holding its identity, so a reference to it
can be handed out beyond your contacts — posted, mailed, printed on a poster.
Withdrawal stops future changes flowing and does not recall bytes anyone already
has, exactly as `rooms.md` states for references, and the interface must never
imply otherwise.

*Finding* public records — indexes, search across machines, ranking — is
deliberately out of scope. It is a large, largely unsolved design with weak
guarantees available to it, and nothing else here depends on it. Search inside
your own reach (your rooms, your subscriptions, your catalogues) is
`docs/architecture/search.md`'s question and is unaffected.

**Record types are declared by packages and published.** This answers the
question `rooms.md` left open. A type declaration is itself a published thing:
an id, a schema, and the presentation accessors (title, subtitle, thumbnail,
timestamp) that let a generic library view show a book, a playlist and an
article side by side. Two applications that declare the same type interoperate.
The OS knows nothing about `article` and can still display one, which is what
app-owned identity plus system-owned mechanism buys.

## What this fixes

- Adding software stops being a decision, because adding cannot execute code or
  acquire authority. That is what makes casual trial safe, which is the property
  the web has and native platforms lost.
- The gatekeeper disappears. Anyone may publish a package; curation is a
  subscription to somebody's list; the official catalogue has no special powers.
- "Which of my applications contains this vulnerable library" becomes a local
  query with an instant answer, and revocation can target the library.
- The declarative/imperative seam disappears — not by picking a side, but
  because the imperative act writes the declarative document, and the API draws
  the config/state line for every application at once.
- Content gets a distribution story that is the collaboration story, with no new
  nouns: an offer is a reference, and accepting one is the gesture that already
  exists for bringing a record into a room.

## What it costs

**Log witnessing is a protocol commitment.** Gossiping source heads over the
collaboration substrate means the collaboration transport now carries something
security-relevant, and its failure modes become security failure modes. A
machine that never gossips is not attacked, merely unwitnessed — the design must
degrade to "no split-view detection", never to "verification skipped".

**Rebuilder attestations only mean something if rebuilders exist.** The count in
the listing is a real number or it is a lie. This needs at least one independent
rebuilder that is not us, and until then the honest display is a count of one.

**Handoff grants require every capability-bearing gesture to be OS-drawn.** The
moment an application draws its own file picker, the implied-grant model has a
hole in it and prompts come back. This is the same constraint that already
applies to window chrome and search.

**A syntax-tree CRDT is more machinery than a value map.** Node identity,
comment attachment, and diffing a parse against a tree are real work, and
getting the merge semantics wrong is worse than not offering text editing at
all — a text editor that silently drops a co-editor's line is a data-loss bug
with a friendly face.

**Reach-requires-relationship costs you strangers.** Nothing can be put in
front of you by someone you have no edge to, which is the anti-spam property and
also means first contact needs an out-of-band introduction. With public
discovery out of scope, a stranger's work reaches you only if somebody you
already follow passes it along.

**Per-origin network implication needs a real mechanism.** Deriving "this app
may reach this host" from "the user pasted this URL" is easy to state and fiddly
to implement without either leaking a broad grant or breaking redirects, CDNs
and federated services.

## Still open

- **Where a rebuilder's inputs come from.** Reproducing a build requires the
  source and the toolchain to be addressable, which means source itself needs a
  distribution story — probably as records, possibly as packages.
- **Whether budgets are enforceable enough to be worth promising.** cgroup
  ceilings are real; "this app used 40 minutes of CPU" is easy; "this app was
  wasteful" is a judgement no mechanism makes.
- **Delegation of reach.** If a person may offer me records, may a list they
  publish do so on their behalf? Subscribing to a list is not obviously
  subscribing to everything its curator wants to hand me.
- **What a room's deletion does to a public record** — `rooms.md` leaves
  orphaned records open, and making one public adds an outside pointer that
  outlives the room.
- **Finding things beyond your own reach.** Deferred, not answered. Whether it
  is ever worth building is a separate question from whether this design is
  right, and nothing above depends on it.

## What exists today

The parts of this that are built, so the gap is visible rather than implied:

- `.tapp` packages: signed root over `manifest_hash ‖ payload_root ‖ publisher
  key`, BLAKE3-addressed, Ed25519 publisher identities, scoped attestors,
  revocation enforced at add *and* every launch
  (`crates/packaging/tilo-app-package`, `install`).
- The sandbox: bwrap plus seccomp plus Landlock, no filesystem paths in the
  manifest, one writable directory per `(room, package)`, capabilities granted
  at runtime (`crates/packaging/launcher`).
- A registry server with an owner key and a publisher allowlist
  (`crates/packaging/registry-server`) — a hosting registry, not yet a log, not
  yet witnessed.
- OS-image reproducibility: digest-pinned base, frozen RPM snapshot,
  `SOURCE_DATE_EPOCH`, verified byte-identical rebuilds, cosign signatures and
  a complete registry SBOM digest-bound in logged provenance, and `bootc upgrade` refusing unsigned images
  from the official Tilo repositories; other references fall to the policy's
  permissive default (`docs/supply-chain.md`, `docs/security/defects.md` P2-6).

Not built: source logs and witnessing, rebuilder attestations, the Profile,
offers, public records, budgets, and per-origin network implication.
