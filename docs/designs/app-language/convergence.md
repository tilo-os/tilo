# Convergence

How resources stay correct when people and agents change them at the same time, offline, or
when the resources live on outside services.

## 1. Where a resource lives

Every resource lives somewhere, and that place has the final say on it:

| Resource | Lives in | Final say |
|---|---|---|
| A note, a todo, a card, a file in Tilo | the workspace | the workspace's host |
| A mail, a draft | the mail account | the mail server |
| A video, a playlist | YouTube | YouTube |
| A feed entry | the workspace (the feed only delivered it) | the workspace's host |

- What lives there is true: a mail deleted on the server is gone.
- One resource can mix: a video's title lives on YouTube, whether you watched it lives in the
  workspace.
- What you may change is the lesser of your Tilo grants and what the place allows (you can
  like someone's video, not retitle it). What you can't do isn't offered.

## 2. Actions

- **An action records its arguments.** Every device applies a place's actions in that place's
  order. Actions marked `commutes` (text edits, counter adds, set adds) need no order, so normal
  editing never waits or conflicts.
- **Anything that would differ on replay is recorded like an argument:** a web response, a
  random number, the time. An action says it needs these (`uses web`, `uses random`,
  `uses time`), and the compiler enforces it.
- **What the person saw is an argument.** A default like `done: Bool = not item.done` is
  evaluated when the action is invoked and recorded. Two people ticking the same box both record
  `done: true`, so they agree instead of cancelling out. Positions are named by neighbours
  (`move(item, after: other)`), never counted.
- The compiler rejects the ambiguous forms (writing a field as `not` itself, or itself plus an
  amount) and suggests a recorded default or a counter.

## 3. Provisional, then final

- Your device applies your actions at once, provisionally, and adjusts when the place's order
  arrives.
- Each action has two facts: **final** (provisional → final or rejected, with a reason) and
  **delivered** (this device → the place → the people it concerns → seen by them).
- **`hosted`** means "don't show it as done until the place commits". Use it where order is
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
- An invariant can only cover data in one place. Across places there are flows (§6).
- An outside service's own rules ("attachment too large") come back as the same kind of
  rejection.

## 5. Outside services

A service isn't a separate concept: its resources are types that live there, and the glue is
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
- For mail, YouTube and Drive: getting and searching are capabilities of the resource's place,
  invoked by the system (any view, global or scoped search). What was fetched is cached and
  indexed locally. Writes ("archive", "like") apply at once and become final when the service
  accepts.

## 6. Flows

An action is atomic in one place; nothing is atomic across places. So calling another place's
action is only allowed in a `flow`, a durable sequence of steps:

```
flow book(w: Workshop, p: Principal, card: Card) {
    reserve(w, p)                  // the workspace: hosted, checks seats
    stripe.charge(card, w.price)   // runs once the reservation is final
    mail.send(confirmation(w, p))  // runs once the charge is final
}
```

- Each step runs only once the previous one is final: no charge for a rejected booking.
- Each step carries the flow's id so the place can drop duplicates. Where a service can't
  (SMTP), one device sends, and a crash mid-call is shown as **outcome unknown**, never retried
  silently.

## 7. What people see

Usually nothing: most actions are final within one round trip. Three states are designed
explicitly: **waiting to go online**, **rejected** (with the reason) and **outcome unknown**.
Views choose how prominent delivery and finality are: chat shows delivery on every message, a
payment shows finality.

## 8. Storage and sync

- **The log is the truth.** Each entry holds what was asked (the action and its arguments),
  what it did (its writes), and what it read (field versions).
- **The author's device runs the action;** the place checks that what it read hasn't changed
  since and that the writes are allowed, then orders it. Checking needs versions, not values,
  so it works in encrypted rooms.
- **Everyone else applies the writes** without running app code.
- If something read has changed, the author's device re-runs the action with the same
  arguments and resubmits. People only see a rejection if an invariant now fails.
- Commuting edits skip the check entirely.
- State is a store of current values in paged columns, with the indexes beside it. A new device
  loads a checkpoint and the log after it. History is the log; undo is a new action that
  restores the old values if nobody changed them since.

## 9. Scale

- A device holds what it opened, pinned or searched, and pages in the rest.
- Order is only needed among actions touching the same data, so a host splits the work by
  subtree. Large workspaces spread subtrees across hosts, cut along containment so invariants
  stay in one place, and replicate each part to standbys.
- The host fans each change out once. Presence is sampled and not stored.
- In encrypted rooms the host can't index, so search covers what the device holds.
- Never: one host ordering a whole large workspace, every device holding everything, or
  invariants spanning a workspace.

## 10. Today, and open questions

- Today the signed Automerge change is the truth, with the action recorded beside it for
  History. Moving to §8 replaces it on the hot path.
- Widgets that write documents directly become actions.
- Open: the design of the three exceptional states per app; checkpoint cadence; services that
  can't write conditionally (IMAP replaces a draft by uploading a new copy), where either the
  last write wins and the person is told, or both versions are kept.
