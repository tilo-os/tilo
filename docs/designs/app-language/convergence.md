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

- **Resources are made of resources.** `Text`, `Int`, `Set<T>`, `List<T>`, `Tree<T>`,
  `Collection<T>` and an author's own types are all resources. The built-in ones are written in
  tilolang in `std`. A merge kind is not a separate concept.
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
3. **Versions.** An action records the app version it was invoked under, and replays with that
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
- The same mechanism covers every "the value I saw": a position dropped at, the next status, a
  title as typed.

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

An app's types may be backed by a **source**: the part of the app that connects some of its
resources to the service they live on.

```
/// A video on YouTube.
type Video from youtube {
    title: Text (title)            // YouTube's
    channel: Channel               // YouTube's
    published: Time                // YouTube's
    watched: Bool                  // the workspace's own
}
```

**The source contract,** the same for every service:

| Part | Does |
|---|---|
| query | answers filters and text; declares which filters it answers itself |
| fetch | one resource by id, with its version and this person's permissions on it |
| changes | changes since a cursor, where the service offers them |
| write | an action at the home: a conditional write against a version |

**Query by default.** Nothing is mirrored wholesale, mail included.
- Search plans one query: the local index for the workspace's resources, the sources for theirs,
  merged into one ranking (`search.md`). People and agents just search.
- Whatever was fetched is cached as a real resource stamped with its home's version, indexed
  locally, and refreshed when shown or when it ages.
- Keeping a mailbox or playlist available offline is a person's choice, not the app's.

**Home-owned fields are read-through.** A refresh is not an event. Nothing appears in History
and nobody is credited, because a fetch shows only that a value differs now, not who changed it
or when. Where the service has a history of its own (a thread's messages, a document's
revisions), it is shown as the service's history, read-only.

**Your actions on sourced resources follow §3 unchanged:**
- **Provisional overlay.** "Archive", "save draft" and "file into Sent" apply at once over the
  fetched state, provisionally, and are attributed to you.
- **Committed by the home.** They are final when the service commits, then the service's state
  is simply current. If someone moves the thread back in Gmail's web UI, that is what shows.
- **Merging with the home.** A write is conditional on the version it started from. If the
  home has moved on, Tilo merges both changes from that common base, as for any concurrent
  edit, and writes the result. Text merges like text. A field that can't merge takes the home's
  value, and you are told.
- **Offline.** A draft written offline exists in Drafts at once ("will save when online"). It
  gets the service's id when it is created there, and its own id never changes.

**Identifiers** of a service (`mailto:`, `tel:`, `youtube:`) are written as in `search.md`.
Links between them and people exist only where something actually recorded them.

## 6. Flows: actions across homes

A `flow` sequences actions at different homes.

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
| Save a mail draft offline | the mail account | `type Draft from mail` | Exists at once; saved when online; merged with web edits |
| Archive a thread | the mail account | `archive(t)` | At once; final when the service commits |
| Retitle someone's video | YouTube | nothing | Not offered: the home doesn't permit it |
| Like a video | YouTube | `like(v)` | At once; final when YouTube accepts |
| Book and pay | three homes | a `flow` | Each step runs once the one before is final |

## 9. Consequences for Tilo

- **An action log becomes the source of truth** for workspace-homed resources. It sits above
  Automerge, which keeps storing the commuting kinds (text, counters, sets). Widgets that write
  the CRDT directly become actions: every widget event is already an action reference or a
  binding in `components.md`.
- **The host's commit role** reuses the governance owner's ordered admission.
- **Flows and outcome unknown** reuse the outbox and the native owner's existing unknown-outcome
  state.
- **Sources** reuse account custody for credentials. The filesystem is the workspace's own
  source for files; Drive and others are sources for theirs. Codecs turn formats into
  resources (`format "text/markdown" <-> Document`).

## 10. Open

- The design of the three exceptional UX states, per app.
- **Checkpoint cadence and retention of old app versions** for replay.
- **Sources without conditional writes:** which services offer no version to write against, and
  whether "last write wins, and you are told" is acceptable for them.
