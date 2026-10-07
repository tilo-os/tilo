# Rooms & records — where app state lives

A `.odt` has an obvious identity (a path), obvious scoping (who can read that
path), and obvious versioning. An ebook reader's annotations have none of
those. There is one global library, and "share my notes on chapter 3" is not
expressible. Same for a video app's subscriptions, a weather app's cities, a
music app's playlists.

That asymmetry is not cosmetic. It is why the collaboration story stops at the
edge of the filesystem.

## Where this started, and what remains

Originally an app's entire state was one flat CRDT addressed by `(package_id,
workspace_uuid)`. Two people editing two different `.odt` files in one
workspace merged each other's whole document, opening a second PDF replaced the
first one's page-keyed annotations, and "share this document" could not be
expressed.

Records are now the routing key for apps that adopt them: a connection names
its record in `Hello`, the broker delivers payloads only to connections on the
same `(app, room, record)` route, and signed envelopes are bound to that route.
The pdf-viewer keeps each document's markup in a record derived from the file
(`markup_record_for` in `apps/pdf-viewer/src/app.rs`), so a second PDF no longer
touches the first one's annotations. Notes uses the canonical record owner.

What remains is the default. An app that does not adopt records still has one
document per `(package_id, room)` — the package document opened through
`auto_hello_env` in `crates/sdk/ui-core/src/collaboration.rs` — and in a shared
workspace that document is the app's collaboration route. Most apps are still
on that path, so for them "share this app in this workspace" is the only
sharing unit.

## The model

**A room is a context. Every context is a room, including your own.**

**An app opened in a room starts empty.** Your library is not an ambient thing
that follows you around — it is the contents of *your* room. A workspace is a
different room, and it begins with nothing in it. Opening the ebook reader in a
project workspace gives you an empty shelf, and that is correct: the room is
the unit of context.

This is the load-bearing decision, and it is what makes the sharing question
disappear rather than merely answering it. There is no "is my library exposed
here?" to reason about, because the shelf in this room is empty until you put
something on it.

**A record is the unit.** Every app declares its state as records — `book`,
`annotation`, `playlist`, `city`, `subscription` — each with a type from the
app's resource model, a stable identity, and a home room.

**A file is a record whose identity is a path.** This is the unification, not a
special case. History, sharing and permissions then have one implementation,
and the file case stops being privileged.

**Records reference other records.** Annotations reference a book rather than
living inside it. So share-the-book-without-my-notes,
share-the-book-with-my-notes, and share-only-my-notes-with-someone-who-already-
has-the-book all fall out of records plus references, with no special cases and
no new vocabulary.

**Bringing your own must be cheap.** A room starting empty is only workable if
moving a record into it is one gesture. If it is a five-click import, people
will work in one room forever and the model collapses back to global. This is a
requirement on the design, not a detail of it.

**The sharing surface is the app's own library view, and the OS draws it.** You
do not visit a permissions app to share a book; you right-click the book,
exactly as you right-click a file. Because the chrome owns the affordance — the
same principle as window decorations and Ctrl+K — it is identical across every
app and no app can get it wrong.

## Not all app state is a record

Your theme, keyboard layout, accounts and TOTP secrets must not reset when you
open a new workspace. Those are properties of **you** or of the **device**, not
of the room.

So there are two categories, and only the first is room-scoped:

- **Content records** — the things an app makes, collects or imports. Room
  scoped. Empty in a new room.
- **Person/device settings** — identity, credentials, appearance, input.
  Follow the person or the machine.

Naming the split is the point. "Which of my state is per-room?" is exactly the
question that otherwise gets answered inconsistently once per app.

## What this fixes

- Per-document collaboration, rather than per-app.
- The pdf-viewer annotation loss, structurally — annotations reference a
  document record, so a second document cannot clobber the first's.
- "Share one file with someone who is not a collaborator": put that record in a
  room they are in, or make a room containing only it.
- Ebook annotations, YouTube subscriptions and weather cities all become the
  same kind of thing as a `.odt`, with the same sharing and history story.

## What it costs

**Per-record CRDT routing is a protocol change, not a refactor.** The broker
routes opaque signed bytes and never materialises them —
`crates/session/shell/src/shell/collab_broker.rs` holds no `AutoCommit` at all.
Of the two ways to make the record the routing key (the broker understands
record boundaries, or apps announce a channel per record) the second was
chosen: the record is the third component of the route key.

Related, and already learned the hard way in `crates/session/shell/src/network/replica.rs`: an
automerge change graph **cannot be served in part**. Changes carry their
`deps`, so filtering a stream leaves the receiver with changes whose
dependencies never arrive, parked forever. Per-record sharing therefore implies
per-record *documents*, not a filtered view of one document.

## Rooms hold references, not copies

**A record exists once. Rooms reference it.**

This is the load-bearing answer to "can a record live in two rooms". Copying is
simpler and it throws away the shared-edit property that is the entire reason
the state is a CRDT — two copies of a book diverge and no amount of merging
makes them one book again. So: the record *is* the document, identified by its
record id, and a room holds a list of record ids. Bringing a record into a room
adds a reference; it moves no bytes. Edits made through either room are edits to
the one document.

**The reference is what grants access.** A member of a room can reach the
records that room references, and nothing else. Removing the reference removes
the access — with the honest caveat that a peer who already synced the bytes has
them, so removal stops future changes flowing, it does not un-share history.
That limit is inherent to giving someone data, not a flaw in the model, and the
UI must not imply otherwise.

**Copying still exists, as a different verb.** "Duplicate here" forks a new
record id with a provenance link to its origin. Two verbs, two names, two
outcomes — the user is never left guessing which one a gesture did.

## Bringing your own is a search result

The gesture has to be one step or the clean-slate default collapses back to
global (see above). It already exists: **search**.

Super+K, type the book's name, and results include records from your other
rooms — labelled with where they live. Activating one brings it into this room.
No import dialog, no file picker, no new vocabulary: the gesture for *opening*
something and the gesture for *bringing* it are the same gesture, which is why
it is cheap enough to rely on.

This also settles what search is for. It is not a file finder that happens to
list apps; it is the way you reach any record from anywhere, and provenance is
part of the result row. Drag onto a workspace spoke in the wheel disc stays as
the bulk gesture, for when you are moving several things and already have them
in front of you.

## History nests: room outside, record inside

A room's own history is the log of actions on the room **as a resource** —
references added and removed, members joined and left, capabilities granted and
revoked. A record's edits belong to the record's history.

The room timeline shows those edits as roll-ups — *"Alice made 12 changes to
Chapter 3"* — that expand into the record's own history. That is the
expand/collapse structure the history surfaces already want, arrived at from the
data model rather than bolted onto it: the room timeline is the outer level, the
record history the inner one, and the filters (contributor, action type, date
range, app) mean the same thing at both levels.

## Your own room is not special

Same kind of room, one member. It is where new records land by default, and that
is the only thing that distinguishes it — a default, not a rule. The shell opens
it when no workspace is selected, which is a UI choice.

Making it a distinguished kind would reintroduce exactly the asymmetry this
document exists to remove: the moment "personal" is a different sort of thing
from "shared", every feature has to be built twice and sharing becomes a
migration instead of a reference.

What the person keeps for themselves alone — watch history, the words the
keyboard learned — lives in the **person's own room**: a room like any other,
except that its members are the person's certified devices by derivation from
their root key, with no roster to invite anyone into. An app keeps such a
record beside the one it shows, in whatever room that is
(`docs/architecture/storage.md` §13.9, "Person records").

## An app's disk is per-room; its index is not

"An app opened in a room starts empty" holds on disk. An app does not name a
writable path — the launcher gives it one, at
`<data home>/tilo-rooms/<room>/<package>`, bound at `/run/tilo/app` with `HOME`
and the `XDG_*` variables pointed inside it. Two rooms are two directories, so
`cli::standalone_state_dir`, `dirs::home_dir()` and a hard-coded
`~/.local/share/<package>` all partition by room without an app changing a
line, and there is no manifest field with which to opt out. A launch with no
workspace gets the `personal` room, which is a room like any other.
`crates/packaging/launcher/tests/room_confinement.rs` proves it: a record
written in room A is `ENOENT` in room B, under a real `bwrap`.

The room *index* is a separate question, and it is the compositor's — one
document per room, which is where a fact about a room belongs anyway. The gap
that leaves is worth naming: **an app launched into a room does not read that
room's index.** A record's own `home` stamp is what carries its room across the
gap, which is why a record is attributed to its home room even when no index
references it.

## Still open

- Whether a record's *type* is app-owned or system-owned. App-owned is less
  coordination; system-owned is what would let one library view show a book, a
  playlist and a document side by side.
- What happens to a room's references when the room is deleted. Orphaned records
  need a home, and "your own room" is the obvious one — but silently moving
  another person's records there is wrong.
