# Convergence

How resources stay correct when people and agents change them at the same time, offline, or
when the resources live on outside services.

## 1. Origins

Every resource has an **origin**: the server with the authoritative copy, which orders and
accepts its changes. Everything else holds a replica or a cache.

| Resource | Origin |
|---|---|
| A note, a todo, a card, a file in Tilo | the workspace host |
| A mail, a draft | the IMAP server |
| A video, a playlist | YouTube |
| A feed entry | the workspace host (the feed only delivered it) |

- The origin's state is authoritative: a mail deleted on the server is gone.
- Fields can have different origins: a video's title has YouTube as origin, whether you watched
  it has the workspace host.
- Permission is the lesser of your Tilo grants and what the origin allows (you can like
  someone's video, not retitle it). Actions you can't perform aren't offered.

## 2. Actions

- **An action records its arguments.** Every device applies an origin's actions in that origin's
  order. Actions marked `commutes` (text edits, counter adds, set adds) need no order, so normal
  editing never waits or conflicts.
- **Anything that would differ on replay is recorded like an argument:** a web response, a
  random number, the time. An action says it needs these (`uses web`, `uses random`,
  `uses time`), and the compiler enforces it.
- **What the person saw is an argument.** A default like `done: Bool = not item.done` is
  evaluated when the action is invoked and recorded. Two people ticking the same box both record
  `done: true`, so they agree instead of cancelling out. Positions are the same: a move
  may name a neighbour (`move(item, after: other)`) or a place (`move(item, to: 2)`, as a drag
  gives one), and a place is resolved when the action runs, against the list the person saw,
  to the item it then comes before. The move is recorded beside that neighbour, so moves made
  elsewhere at the same time leave each item where its mover meant.
- The compiler rejects the ambiguous forms (writing a field as `not` itself, or itself plus an
  amount) and suggests a recorded default or a counter.

## 3. Provisional, then final

- Your device applies your actions at once, provisionally, and adjusts when the origin's order
  arrives.
- Each action has two facts: **final** (provisional → final or rejected, with a reason) and
  **delivered** (this device → the origin → the people it concerns → seen by them).
- **`hosted`** means "don't show it as done until the origin accepts it". Use it where order is
  worth gaming (the last seat), where time must be trusted (deadlines), or where it can't be
  taken back (paying, sending).
- Membership and permissions are always decided by the platform, never by an app.

## 4. Invariants

```
type Workshop {
    seats: Int = 12
    attendees: Set<Principal>
    invariant attendees.count <= seats "{title} is full"
}
```

- An action that would break an invariant of anything it touches is rejected, the same way
  everywhere, and its author is told why.
- Invariants come with the type: reuse a `Workshop` and its capacity rule comes along.
- An invariant can only cover data with one origin. Across origins there are flows (§6).
- An action states what it needs of its subject with `require`, the action's own invariant:

  ```
  primary action send(d: Draft) {
      require not d.sent "{d.subject} went already"
      ...
  }
  ```

  A requirement is pure and reads only the subject, so wherever the action would be offered on
  it (a window's header, a menu, an agent's choices) the system leaves it out while one fails,
  and an invocation anyway is rejected with the reason, the same way everywhere, before it
  writes anything.
- An outside service's own rules ("attachment too large") come back as the same kind of
  rejection.

## 5. Outside services

A service isn't a separate concept: its resources are types whose origin is the service, and the glue is
ordinary actions that `use web`.

```
/// Fetches the feed and takes in what is new or edited.
action refresh(feed: Feed) uses web {
    let r = web.get(feed.url, if_none_match: feed.etag)
    if r.status == 304 { return }
    if not r.ok { feed.trouble = "{r.status_text}"; return }
    for e in syndication.parse(r.body).entries {
        feed.entries.upsert(key: e.id, if_newer: e.updated) { title: e.title, summary: e.summary }
    }
}
```

- `upsert(key:)` keys resources by the service's id, so nothing appears twice.
- The service's fields are written only by the service's actions, and people's fields (`read`,
  `starred`) only by people, so refreshing never undoes what someone did.
- A failure is state (`trouble`), shown on that feed; it never blocks others.
- When it runs is a trigger (a default shortcut such as "every 30 minutes"), not syntax.
- For mail, YouTube and Drive: getting and searching are capabilities of the resource's origin,
  invoked by the system (any view, global or scoped search). What was fetched is cached and
  indexed locally. Writes ("archive", "like") apply at once and become final when the service
  accepts.

## 6. Flows

An action is atomic at one origin; nothing is atomic across origins. So calling another origin's
action is only allowed in a `flow`, a durable sequence of steps:

```
flow book(w: Workshop, p: Principal, card: Card) {
    reserve(w, p)                  // the workspace: hosted, checks seats
    stripe.charge(card, w.price)   // runs once the reservation is final
    mail.send(confirmation(w, p))  // runs once the charge is final
}
```

- Each step runs only once the previous one is final: no charge for a rejected booking.
- Each step carries the flow's id so the origin can drop duplicates. Where a service can't
  (SMTP), one device sends, and a crash mid-call is shown as **outcome unknown**, never retried
  silently.

## 7. What people see

Usually nothing: most actions are final within one round trip. Three states are designed
explicitly: **waiting to go online**, **rejected** (with the reason) and **outcome unknown**.
Views choose how prominent delivery and finality are: chat shows delivery on every message, a
payment shows finality.

An outside effect (`outside("smtp.send", ...)`) is marked as being handed over, as a change of
its own, before it is handed to the service, so a device that stops in between shows it as
outcome unknown and never sends it twice. A service that cannot be reached leaves it waiting to
go online, with the reason; it is tried again when the network comes back (the platform's
network status, not a timer). A refusal is final, with the service's reason. Code reads where
it stands with `delivery(r)`: `.none` until the resource's actions queued one, else its `state`
(`waiting`, `sending`, `sent`, `rejected`, `unknown`) and `reason`. Mail shows it on the draft.

## 8. Storage and sync

- **The log is the truth.** Each entry holds what was asked (the action and its arguments),
  what it did (its writes), and what it read (field versions).
- **The author's device runs the action;** the origin checks that what it read hasn't changed
  since and that the writes are allowed, then orders it. Checking needs versions, not values,
  so it works in encrypted workspaces.
- **Everyone else applies the writes** without running app code.
- If something read has changed, the author's device re-runs the action with the same
  arguments and resubmits. People only see a rejection if an invariant now fails.
- Commuting edits skip the check entirely.
- State is a store of current values in paged columns, with the indexes beside it. A new device
  loads a checkpoint and the log after it. History is the log; undo is a new action that
  restores the old values if nobody changed them since.

### Items

- **An item is stored by its id.** A document stores its items flat, each under its id, and an
  item's place is a field of the item, its *placement*: the item holding its collection (or the
  document), the collection's field, and an order key placed beside its neighbours. A
  collection is the items placed in it, by order key, ties by id; the index keeps each
  collection's list, so `for`, `count`, lanes and sorted queries read it as before.
- **A reference is the id** (`doc/item`). It stays valid when the item moves to another
  collection, and history follows the one item through its moves. Moves stay within the
  document, the unit an origin orders.
- **Concurrent changes merge:**
  - The placement is one register. Of two concurrent moves of an item, the last in the record's
    order wins: parent, field and place together, never a mix of both.
  - A field edit writes the item, not its placement, so an edit made while someone moves the
    item is kept, wherever the item ends up.
  - Removing an item writes its placement as removed; everything placed in it goes with it.
    Nothing is deleted from the record, so undoing the removal brings it back with all it held.
  - An item moved out of a container before it is removed, or at the same time, is no longer in
    it and stays where it was moved. An item moved or inserted into a container while it is
    removed goes with it, as an edit inside it does. A removal and a move of the same item: the
    last wins.
  - Concurrent moves can put two items inside each other. In such a cycle the move that is last
    in the record's order does not take effect, and its item stays where that move took it from
    (the placement records it). Should that place be in a cycle too, the item is in no
    collection until someone moves it. A single action that would move an item into itself is
    refused.
- **Records written before items were flat** held each item inside the collection it was
  inserted into. They are read where they are: each nested item by its id, placed in the
  collection and at the order key it has, until a move or removal writes its placement into its
  map. Nothing is copied or rewritten, so no edit is lost to the migration and every id stays.
  An entry that does not read (an id held twice, an entry without an order key) is set aside:
  left where it is, in no collection. A reference written as a path (`doc/field:item/...`)
  names the item of its last step.

## 9. Scale

- A device holds what it opened, pinned or searched, and pages in the rest.
- Order is only needed among actions touching the same data, so a host splits the work by
  subtree. Large workspaces spread subtrees across hosts, cut along containment so invariants
  stay at one origin, and replicate each part to standbys.
- The host fans each change out once. Presence is sampled and not stored.
- In encrypted workspaces the host can't index, so search covers what the device holds.
- Never: one host ordering a whole large workspace, every device holding everything, or
  invariants spanning a workspace.

## 10. Today, and open questions

- Today the signed Automerge change is the truth, with the action recorded beside it for
  History. Moving to §8 replaces it on the hot path.
- Widgets that write documents directly become actions.
- Open: the design of the three exceptional states per app; checkpoint cadence; services that
  can't write conditionally (IMAP replaces a draft by uploading a new copy), where either the
  last write wins and the person is told, or both versions are kept.
- A package changing where its items live says so itself. The store maps the older layout to the
  flat one; which collection an item belongs in under a new schema is the package's to say, with
  a migration of its own records (authority.md, "Versions"): Board keeps the field that held each
  card's column `(previously)`, and its migration moves the cards of a board written before into
  their columns' collections, each keeping its id and history.
