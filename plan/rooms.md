# Rooms & records — where app state lives

A `.odt` has an obvious identity (a path), obvious scoping (who can read that
path), and obvious versioning. An ebook reader's annotations have none of
those. There is one global library, and "share my notes on chapter 3" is not
expressible. Same for a video app's subscriptions, a weather app's cities, a
music app's playlists.

That asymmetry is not cosmetic. It is why the collaboration story stops at the
edge of the filesystem.

## What is wrong today

An app's entire state is one flat CRDT addressed by `(package_id,
workspace_uuid)` — see `auto_hello_env` in `ui/src/collaboration.rs`. The
consequences are concrete, and all three are current behaviour:

- Two people editing two **different** `.odt` files in one workspace announce
  the identical `(dev.qos.word-processor, room)` pair and merge each other's
  whole document state.
- Opening a second PDF **deletes your annotations on the first**
  (`apps/pdf_viewer/src/doc.rs`, commented there as a known cost — annotations
  are keyed by page number, which only means something for one file).
- "Share this document" is not expressible. Only "share this app in this
  workspace" is.

The tree already names the missing concept, exactly once, in the ebook reader:
`apps/ebook/src/model.rs` — *"`doc_id` — which document (book id here; file
path / doc uuid elsewhere)."* It is a `String` with nowhere to live.

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
- The pdf_viewer annotation loss, structurally — annotations reference a
  document record, so a second document cannot clobber the first's.
- "Share one file with someone who is not a collaborator": put that record in a
  room they are in, or make a room containing only it.
- Ebook annotations, YouTube subscriptions and weather cities all become the
  same kind of thing as a `.odt`, with the same sharing and history story.

## What it costs

**Per-record CRDT routing is a protocol change, not a refactor.** Today the
broker routes opaque bytes keyed on `(app_id, cluster_id)` and never
materialises them — `shell/src/shell/collab_broker.rs` holds no `AutoCommit` at
all. Making the record the routing key means either the broker understands
record boundaries, or apps announce a channel per record. Both are real work
and one of them has to be chosen.

Related, and already learned the hard way in `shell/src/network/replica.rs`: an
automerge change graph **cannot be served in part**. Changes carry their
`deps`, so filtering a stream leaves the receiver with changes whose
dependencies never arrive, parked forever. Per-record sharing therefore implies
per-record *documents*, not a filtered view of one document.

## Open questions

- The gesture for bringing a record into a room. It has to be one step.
- Whether a record can live in two rooms, or only be copied between them.
  Copying is simpler and loses the shared-edit property that motivates the CRDT.
- What a room's *own* history shows once records move in and out of it.
- Whether "personal room" is a room in the same sense, or a distinguished one.
