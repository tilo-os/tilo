# Convergence: resources, homes, order, invariants and flows

How tilolang resources stay correct when people and agents act concurrently, offline, or on
resources that live on outside services. The developer needs six concepts:

1. **Resources, each with a home.**
2. **Actions** that record their arguments and replay in the home's order, with `commutes` for
   the kinds that need no order.
3. **Finality:** provisional, then final or rejected, decided by the home.
4. **`hosted`:** actions that wait for the home instead of applying provisionally.
5. **`invariant`:** checked by the home.
6. **`flow`:** actions sequenced across homes.

Everything else is the platform.

Prior art this combines:
- Bayou: tentative writes, primary commit, rollback and replay;
- Replicache and similar sync engines: mutators recorded as arguments and replayed;
- RedBlue consistency: commuting versus ordered operations;
- invariant confluence, explicit consistency and ECROs: declared invariants, coordination only
  where needed;
- the move-op tree CRDT;
- optimistic concurrency with conditional writes (HTTP preconditions, JMAP `ifInState`).

What is new is putting it in an app language whose types are the synced resources, with
invariants inherited through containment, authority and attribution on every action, and
resources on outside services under the same rules as the workspace's own.

## 1. Resources and their homes

**Every resource has a home: the place that commits changes to it.**

| Resource | Home | Who commits |
|---|---|---|
| A note, a todo, a board card | the workspace | the workspace's host |
| A mail draft, the Sent folder | the mail account | the mail service |
| A YouTube video, a playlist | YouTube | YouTube |
| A file in a Drive folder | Drive | Drive |
| A file in Tilo's filesystem | the workspace | the workspace's host |
| A feed entry | the workspace (the feed only contributed it) | the workspace's host |

- **Resources are made of resources.** `Text`, `Int`, `Set<T>`, `List<T>`, `Tree<T>`,
  `Collection<T>` and an author's own types are all resources. The built-in ones are written in
  tilolang in `std`. A merge kind is not a separate concept.
- **The home is trusted about existence.** A mail deleted on the server is gone. A feed entry's
  home is the workspace from the moment it arrives: the feed may add and edit entries, but its
  dropping one means nothing. Which home a type has is the whole difference between a mailbox and
  a feed; there is no separate catalogue or stream concept.
- **One resource can have fields with different homes.** A `Video`'s title lives on YouTube.
  Whether *you* watched it is the workspace's own field. Each field follows its home's rules
  (§5).
- **The home decides what may change.** Your authority over a resource is the lesser of your
  Tilo grant and the home's permission, as authority follows authorship everywhere. You may
  like someone's video but not retitle it; your own upload's title is yours to change. The home
  reports this per resource, and actions you can't perform are not offered to people or agents.

## 2. Actions

1. **An action records its arguments; its body replays.** Every replica applies a home's
   actions in that home's order, and each action runs against the state at its position.
2. **`commutes`.** An action declared `commutes` is checked by the compiler and tests to commute
   with others of its kind, and is never reordered. Text splices, counter adds and set adds are
   `commutes` in `std`, so ordinary editing never replays.
3. **Non-deterministic inputs are recorded like arguments.** An action that reads something
   whose answer differs on replay says so: `uses web`, `uses random`, `uses time`. What it read
   (the response, the number, the time) is recorded with the invocation, exactly like an
   invocation-time default. Replay runs the body again at its position with those recorded
   inputs, so the action stays deterministic and is checked against the state at its position.
   Large responses are stored as content-addressed blobs. The compiler's effect checking
   enforces the declaration: `web.get`, `random()` and `now()` can only be called from actions
   that declare them.
4. **Versions.** An action records the app version it was invoked under, and replays with that
   version. Migrations apply to the action log as well as to state.

### 2.1 Outcomes and deltas

A field's type defines its writes and how each one merges:

| Write | On | Merges as |
|---|---|---|
| `=` | a register (`Bool`, `Text merge register`, enums, references) | outcome: the value at the action's position is the value written |
| `+=`, `-=` | a counter | commuting delta |
| `add`, `remove` | `Set<T>` | commuting |
| splices | `Text` | commuting |
| `insert`, `remove`, `move` | `Collection<T>`, `List<T>`, `Tree<T>` | the collection's own rules; `Tree` refuses cycles |

A register written with a value computed from that same register means "recompute when
applied". For `item.done = not item.done`, two people ticking the same item at once cancel out,
which is never what the tick meant.

**What the person saw is an argument.** An action parameter may have a default, written in
terms of the earlier parameters. The default is evaluated **when the action is invoked**, on
the invoker's view, and recorded as the argument. Replay never re-evaluates it.

```
/// Checks or unchecks an item.
action check(item: Todo, done: Bool = not item.done)
    (label: if done { "Check" } else { "Uncheck" }, icon: .check, on: item.done) {
    item.done = done
}
```

- The checkbox, the header, the context menu, the keyboard and an agent all call the same
  action. People get the default; an agent may pass `done:` explicitly.
- Two people checking at once both record `done: true`.
- The same mechanism covers every "the value I saw": the next status, a title as typed, and a
  position dropped at, which is named by its neighbour, not counted:
  `move(item, after: other)`. A counted position goes stale as soon as anyone inserts or sorts.
  The store already takes named places (`At::End`, `After(id)`, `Before(id)`).

**The compiler rejects the ambiguous forms:** a register written as `not` of itself, or itself
plus or minus an amount. The error names the two fixes: a parameter with an invocation-time
default (an outcome), or a counter (a delta). `title = trim(title)` and other legitimate
replays are not reported.

## 3. Order and finality: the home commits

- **Devices apply their own actions at once,** in a provisional order. They rebase onto the
  home's committed order as it arrives, rolling back and replaying provisional actions behind it.
- **When the workspace's host is unreachable,** devices that sync directly use a deterministic
  order (causal order, then a tiebreak), so they agree with each other until the host commits.
- **Every action carries two facts:**
  - **Final:** provisional, then final or rejected, decided by the home. A rejection carries its
    reason, from an invariant, a permission or the service, and its dependents (§4).
  - **Delivered:** this device, then the home, then the people it concerns, then seen by them.
- **"May not have taken effect"** appears only when the commit is overdue relative to the
  device's known round trip to that home. Offline, the state is "will apply when you're back
  online".
- **`hosted`: wait for the home.** On a type, or on a single action, `hosted` means **no
  provisional effect**: nothing shows as done until the home commits. Use it when:
  - the outcome needs **trusted time** (deadlines, auctions);
  - parties have an **interest in gaming the order** (selling to strangers, public rooms);
  - the action **can't be taken back**: paying, sending.

  Saving a draft applies provisionally; paying is `hosted`.
- **Membership and permissions** are hosted by the platform already, through the governance
  owner. An app never decides them.
- **Very late arrivals.** An action from a device that was offline past the home's last
  checkpoint is placed after the checkpoint, as if done now, and checked there. Its author is told
  if it was rejected. Compacted history is never replayed into.

## 4. Invariants

```
type Workshop {
  title: Text
  seats: Int = 12
  attendees: Set<Principal>
  invariant attendees.count <= seats "{title} is full"
}

type Conference {
  workshops: Collection<Workshop>
  invariant no person in two workshops at the same slot "You're already in {other} then"
}
```

- **Rejection.** An action whose effects would break an invariant of any resource it touches,
  including every resource that contains it, is **rejected at its position** by the home,
  identically everywhere. Its message is shown to its author.
- **Inheritance.** Invariants come with the type. `std`'s `Tree` refuses cycles, and a
  `Workshop` reused in another app keeps its capacity rule.
- **Scope.** An invariant may reference anything with the same home. Across homes there are no
  invariants, only flows that ask.
- **A service's own rules are its invariants.** "Attachment too large" or "video no longer
  exists" come back as the same rejection with a message.
- **Preconditions.** `require cond else "message"` in an action body is for preconditions that
  belong to the action, not the type.
- **Dependents.** Actions that replay and fail because an earlier one was rejected are presented
  as **one outcome** with it.

## 5. Resources on outside services

A service is not a new concept. Its resources are types whose home is the service. The glue is
ordinary actions that `use` it:

```
app so.tilo.feeds "Feeds" (icon: .rss, category: .news)

/// A feed someone in this workspace subscribed to.
type Feed {
    url: Url
    title: Text (title)
    entries: Collection<Entry>
    etag: Option<Text>                    // the server's cache validators
    modified: Option<Text>
    trouble: Option<Text>                 // why the last fetch failed, shown on the feed
}

/// One post, as the feed published it. Its home is the workspace.
type Entry {
    guid: Text
    title: Text (title)
    link: Option<Url>
    published: Time
    updated: Time
    summary: Text (body)
    read: Bool = false
    starred: Bool = false
}

/// Fetches the feed and takes in what is new or edited.
action refresh(feed: Feed) uses web {
    let r = web.get(feed.url, if_none_match: feed.etag, if_modified_since: feed.modified)
    if r.status == 304 { return }
    if not r.ok { feed.trouble = "{r.status_text}"; return }
    let doc = syndication.parse(r.body, r.content_type)       // a library, not the runtime
    feed.title = doc.title
    feed.etag = r.header("etag")
    feed.modified = r.header("last-modified")
    feed.trouble = .none
    for e in doc.entries {
        let updated = e.updated ?? e.published ?? now()
        feed.entries.upsert(key: e.id ?? e.link ?? hash(e.title, e.published), if_newer: updated) {
            title: e.title, link: e.link, published: e.published ?? updated,
            updated: updated, summary: e.summary
        }
    }
}
```

- **Identity.** `upsert(key:)` keys resources by the service's id. The same post never appears
  twice, and an edited one updates in place.
- **No conflicts by construction.** The service's fields are written only by the actions that
  use it. People write only the workspace's own fields (`read`, `starred`), so refetching never
  resets them.
- **Failures are state.** `trouble` shows on the feed, like the connecting state. One broken feed
  never blocks the others.
- **Arrivals are the action's writes.** Nobody else did anything, and "new" is `read = false`.
- **When it runs is a trigger, not syntax.** Main's saved shortcuts already have schedule
  triggers (with a catch-up policy), app-opened and record-changed triggers. An app ships a
  default shortcut ("refresh every 30 minutes"). Shortcuts are armed today only on their saver's
  device. A workspace-owned shortcut armed on the host, so a refresh runs once for everyone, is
  the one extension needed.

**Resources whose home is the service** (mail, YouTube, Drive) use the same means:
- **Reads.** Queries and fetches are actions that `use` the service. Search plans one query over
  the local index and these, merged into one ranking (`search.md`). What was fetched is cached as
  a real resource stamped with the home's version, indexed locally, and refreshed when shown or
  when it ages. A refresh writes only home-owned fields; it is not an event in History.
- **Writes.** "Archive", "save draft" and "like" apply at once over the fetched state,
  provisionally, attributed to you, and are final when the home commits (§3). Offline, they stay
  pending, not rejected. Only a permanent refusal by the home ("attachment too large") is a
  rejection.
- **Merging with the home.** Where the service can write conditionally ("only if still the
  version I saw"), Tilo merges both changes from the common base when the home has moved on, then
  writes the result. Where it can't (IMAP replaces a draft by uploading a new copy), see §10.
- **The home's permission.** Your authority over a resource is the lesser of your Tilo grant and
  what the service allows for that resource, reported with each fetch. Actions you can't perform
  aren't offered.
- **Accounts are optional where the service allows.** Public data comes from public endpoints;
  signing in is needed only for the person's own data and writes.

**Identifiers** of a service (`mailto:`, `tel:`, `youtube:`) are written as in `search.md`.
Links between them and people exist only where something actually recorded them.

## 6. Flows: actions across homes

A `flow` is a durable sequence of atomic actions at different homes. Every action already goes
over the network; what a flow adds is the honest answer to atomicity. An action is atomic at
one home, and nothing is atomic across homes. **Calling another home's action is allowed only in
a flow;** inside a plain action the compiler refuses it, because it would pretend to be atomic.
In the literature this is a saga.

```
flow book(w: Workshop, p: Principal, card: Card) {
    reserve(w, p)                          // the workspace: hosted, invariant on seats
    stripe.charge(card, w.price)           // Stripe: runs once reserve is final
    mail.send(confirmation(w, p))          // the mail account: runs once the charge is final
}
```

- **A step runs only once the steps before it are final.** Money never moves for a booking that
  is later rejected.
- **Each step carries the flow's invocation id as its idempotency key.** Any device may deliver
  it. The home deduplicates it, checks its own invariants and the caller's authority, and
  answers.
- **A home without idempotency** (SMTP, many legacy systems) gets one executor, through the
  outbox. The outbox records "sending" durably before the call. A crash mid-call makes the step
  **outcome unknown**: shown to a person, never silently retried.
- **Put an invariant where the thing it protects lives.** A trip budget that is real money is a
  bank sub-account or a spending mandate. It is not a second invariant in the trip competing
  with the bank's balance.

There is no separate concept of an "outside action". Every effect is an action at some home.

## 7. What people see

The two facts of §3 are the whole vocabulary. Views decide how prominent they are:
- **Chat** shows delivery on every message.
- **A note** shows it only when it is stale and someone else is looking.
- **A booking or a payment** shows finality prominently.

When homes are reachable, nearly every action is final within one round trip and no state is
shown. Three exceptional states are designed explicitly, the same whatever the home:
- **pending offline;**
- **rejected,** with its reason;
- **outcome unknown.**

**Approval** follows from an action's danger level and the viewer's authority, decided centrally.
An action may supply a **confirmation view**: an ordinary view selecting the values that make the
choice reasonable. The person acting sees it as a modal; others see the pending action with the
same view.

## 8. The cases

| Case | Home | The developer writes | Outcome |
|---|---|---|---|
| Tick a todo | workspace | `check(t, done: Bool = not t.done)` | Converges, offline, never rejected |
| Like a post | workspace | `likes.add(1)` | All count |
| Chat message | workspace | `messages.append(..)` | Never rejected; delivery shown |
| Move a folder | workspace | `n.parent = into` on a `Tree` | A cycle is rejected with its reason |
| Workshop signup | workspace | `hosted`, `invariant attendees.count <= seats` | The host decides the last seat; the loser is told |
| Assignment deadline | workspace | `hosted type Course`, `invariant submitted_at <= due` | The host's clock decides |
| Removing a member | workspace | nothing (platform, hosted) | Later edits by that member are refused |
| Save a mail draft offline | the mail account | `save(d)` | Exists at once; saved when online; merged with web edits |
| Archive a thread | the mail account | `archive(t)` | At once; final when the service commits |
| Retitle someone's video | YouTube | nothing | Not offered: the home doesn't permit it |
| Like a video | YouTube | `like(v)` | At once; final when YouTube accepts |
| Book and pay | three homes | a `flow` | Each step runs once the one before is final |

## 9. Storage and sync: intent, effect, and validated reads

**The log is the truth for workspace-homed resources.** Each entry holds the intent and the
effect, and the home validates what the action read: optimistic concurrency, as in FoundationDB's
resolver, applied to app actions.

```
id, author, signature
intent:  check(item 7f3, done: true), app version, recorded inputs (time, web, random)
reads:   { item7f3.done @ v41 }                 # field versions the body read
writes:  { item7f3.done = true }                 # its effect; text, counter and set edits as CRDT ops
```

1. **The author executes.** The device runs the action against its current state. The runtime
   records reads and writes (it already tracks every field a body reads), shows the result at
   once, and sends the entry.
2. **The home validates and orders.** It commits the entry if everything it read is still at the
   version read and its writes are within the author's grants, and gives it a position.
   Validation needs field keys and versions, never values, so it works in end-to-end encrypted
   rooms.
3. **Everyone else applies the effect.** Receivers apply the writes and run no app code. There is
   no cross-version determinism problem, and old app versions are not kept for replay.
4. **Stale reads are the author's to settle.** If something read has changed, the home answers
   "stale". The author re-executes with the same recorded arguments and inputs against the new
   state, and resubmits. People see nothing, unless the re-execution now breaks an invariant:
   that is the rejection with its reason. Invariants are serializable without the host running
   code.
5. **Commuting kinds skip validation.** Text splices, counter adds and set adds are CRDT
   operations with no read set, so ordinary editing never conflicts or waits. Text follows an
   event-graph design (Eg-walker): the log of text operations is the truth, the state is plain
   text, and CRDT structure exists only while merging concurrent edits.

**State is a materialized store, not a document:**
- **Storage.** Current field values sit in paged on-disk columns, together with the indexes
  `search.md` §4 describes. They are the same store.
- **Opening.** Opening reads the pages it shows.
- **A new device** loads the home's signed checkpoint and the log after it.
- **Compaction.** Entries before a checkpoint are compacted to their intents, which History keeps.

**What follows from it:**
- **History** is the log of intents.
- **Undo** is a new action whose writes restore the previous values. Its reads ensure nobody
  changed them since.
- **Permissions** are checked once per entry, against its write set.
- **`hosted`** means: don't show the effect until it is committed.
- **Services** use the same mechanism. A conditional write is "my reads are still current", with
  the service's version tag as the version.
- **Sync** is "entries after position N", per home: ordered, resumable, with no growing actor sets.

**Hard parts:**
- **Read granularity.** `attendees.count <= seats` reads the membership of a set, not one field.
  Reads need ranges and predicates, like FoundationDB's conflict ranges. Too coarse conflicts
  needlessly; too fine costs validation time.
- **What the host learns.** In encrypted rooms it sees which fields change and when, never
  their values. That metadata is stated, not hidden.
- **Without a host,** peers have only a provisional, deterministic order. Commuting edits are
  unaffected. Anything with an invariant waits for a home, which is `hosted` again.
- **Structured rich text** (blocks, marks, moves) in the event-graph style is real work. It is
  where Automerge is more mature today.

## 9.2 Scale: many people, much data, many hosts

The model scales because a home is per resource, not per workspace. Three things make it so:

1. **Partial replication.** A device holds what it opened, pinned or searched, plus the index,
   and pages in the rest. Checkpoints are per subtree (a notebook, a channel), so a new device
   loads what it needs.
2. **Ordering partitioned by resource.** The order only has to be total among entries that touch
   the same data. A host partitions validation by subtree, as FoundationDB partitions resolvers
   by key range. Most actions touch one resource and stay on one partition. Hot spots are
   only where they are inherent: a hot counter commutes, and the last seat is serial by nature.
3. **Partitioned and replicated homes.** A large workspace's subtrees are spread across hosts,
   each the home for some of them. Shards are cut along containment, so a type's invariants
   stay within one home; anything spanning homes is a flow. Each partition's log is replicated
   to standby hosts, and consensus picks who continues when one fails. Apps never see it.

**Fan-out.** With thousands of people, the host fans each change out once, rather than every
device sending it to every other device. Each device receives what it subscribed to. Presence is
ephemeral and sampled.

**Limits:**
- **Search over data a device doesn't hold.** In unencrypted rooms the host keeps the index. In
  end-to-end encrypted rooms it can't, and search covers what the device holds.
- **What must never be built:**
  - one host ordering a whole large workspace;
  - every device holding everything;
  - invariants spanning a workspace.

## 9.1 Consequences for Tilo

- **Today the Automerge change is the truth.** It is signed with claims (which action, which
  storage units). tilolang records the invocation beside it for History. Moving to §9 replaces
  Automerge on the hot path with the log and the materialized store. The claims already carry
  half of an entry (action and write set).
- **Widgets that write the CRDT directly become actions.** Every widget event is already an
  action reference or a binding in `components.md`.
- **The host's validation** extends the governance owner's ordered admission and the existing
  claim checks.
- **Flows and outcome unknown** reuse the outbox and the native owner's existing unknown-outcome
  state.
- **Services** reuse account custody for credentials. The filesystem is the workspace's own home
  for files. Codecs turn formats into resources (`format "text/markdown" <-> Document`).

## 10. Open

- The design of the three exceptional UX states, per app.
- **Checkpoint cadence.** Old app versions are no longer needed for replay (§9): receivers apply
  effects, and only an author re-executes its own uncommitted actions.
- **Services without conditional writes.** IMAP can't edit a draft in place: a client uploads a
  new copy and deletes the old one, with no check for an edit made elsewhere meanwhile. For such
  services: either the last write wins and the person is told, or both copies are kept as
  conflicting versions.
