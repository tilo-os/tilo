# Convergence: resources, order, invariants and effects

How tilolang resources stay correct when people and agents act concurrently, offline, or on
systems outside the workspace. The developer needs five concepts:

- resources made of resources;
- `invariant`;
- `hosted`;
- `commutes` (mostly inside `std`);
- `flow` with foreign and outside calls.

Everything else is the platform.

Prior art this combines:
- Bayou: tentative writes, primary commit, rollback and replay;
- Replicache and similar sync engines: mutators recorded as arguments and replayed;
- RedBlue consistency: commuting versus ordered operations;
- invariant confluence, explicit consistency and ECROs: declared invariants, coordination only
  where needed;
- the move-op tree CRDT.

What is new is putting it in an app language whose types are the synced resources, with
invariants inherited through containment, and with authority and attribution on every action.

## 1. Resources converge through their actions

1. **Resources are made of resources.** `Text`, `Int`, `Set<T>`, `List<T>`, `Tree<T>`,
   `Collection<T>` and an author's own types are all resources. The built-in ones are written in
   tilolang in `std`. A merge kind is not a separate concept.
2. **An action records its arguments; its body replays.** Every replica runs the workspace's
   actions in one order, and each action runs against the state at its position.
   - Whether a write means an outcome or a delta follows from what the action takes.
     `check(t, done)` records the outcome. `like(p)` records intent, which replays as a delta.
   - See §1.1 for how an author says which one they mean.
3. **`commutes`.** An action declared `commutes` is checked by the compiler and tests to commute
   with others of its kind, and is never reordered. Text splices, counter adds and set adds are
   `commutes` in `std`, so ordinary editing never replays.
4. **Versions.** An action records the package version it was invoked under, and replays with
   that version. Migrations apply to the action log as well as to state.

### 1.1 Outcomes and deltas

A field's type defines its writes and how each one merges:

| Write | On | Merges as |
|---|---|---|
| `=` | a register (`Bool`, `Text merge register`, enums, references) | outcome: the value at the action's position is the value written |
| `+=`, `-=` | a counter | commuting delta |
| `add`, `remove` | `Set<T>` | commuting |
| splices | `Text` | commuting |
| `insert`, `remove`, `move` | `Collection<T>`, `List<T>` | the collection's own rules |

An action body replays at its position, so a register written with a value computed from that
same register means "recompute when applied". For `item.done = not item.done`, two people ticking
the same item at once cancel each other out, which is never what the tick meant.

**What the person saw is an argument.** An action parameter may have a default, written in
terms of the other arguments. The default is evaluated **when the action is invoked**, on the
invoker's view, and recorded as the argument like any other. Replay never re-evaluates it.

```
/// Checks or unchecks an item.
action check(item: Todo, done: Bool = not item.done)
    (label: if done { "Check" } else { "Uncheck" }, icon: .check, on: item.done) {
    item.done = done
}
```

- The checkbox, the header's ✓, the context menu, a keyboard shortcut and an agent all call the
  same action. People get the default, the value they saw flipped. An agent may pass `done:`
  explicitly.
- Two people checking the same item at once both record `done: true`. Checking and unchecking at
  once is decided by the order, and the loser sees the result.
- The same mechanism covers every "the value I saw": a position dropped at, the next status after
  the one shown, a title as typed. No toggle, cycle or move concept exists in the language.
- History shows the recorded arguments: "Checked", not "Toggled".

**The compiler rejects the ambiguous forms.** It reports an error for a register write whose
value is `not` of that register, or that register plus or minus an amount. The error names the two
fixes:
- an outcome: a parameter with an invocation-time default;
- a delta: make the field a counter.

Other computations from the same register, such as `title = trim(title)`, are legitimate replays
and are not reported.

## 2. Order: the host commits it

Every workspace has a host.

- **The host commits the order** of every action it receives.
- **Devices apply their own actions at once,** in a provisional order. They rebase onto the
  committed order as it arrives, rolling back and replaying provisional actions behind it.
- **When the host is unreachable,** devices that sync directly use a deterministic order (causal
  order, then a tiebreak), so they agree with each other until the host commits.
- **An action is final when the host has committed it.** A device knows its round trip to the
  host:
  - "may not have taken effect" appears only when the commit is overdue relative to that;
  - offline, the state is "will apply when you're back online".
- **`hosted`** on a resource type means actions touching it are **not applied before the host
  commits them**. They have no provisional effect, and wait while offline. Use it when:
  - the outcome needs **trusted time** (deadlines, auctions), because device clocks decide
    nothing;
  - parties have an **interest in gaming the order** (selling to strangers, public rooms),
    because claims of "I hadn't seen it" mean nothing to the host.
- **Membership and permissions** are hosted by the platform already, through the governance
  owner. An app never decides them.
- **Very late arrivals.** An action from a device that was offline past the workspace's last
  checkpoint is placed after the checkpoint, as if done now, and checked there. Its author is told
  if it was rejected. Compacted history is never replayed into.

## 3. Invariants

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
  including every resource that contains it, is **rejected at its position**, identically
  everywhere. Its message is shown to its author.
- **Inheritance.** Invariants come with the type. `std`'s `Tree` refuses cycles, and a
  `Workshop` reused in another app keeps its capacity rule.
- **Scope.** An invariant may reference anything in the same workspace, whether or not it is on
  this device. When it isn't, the action stays provisional until the host commits it. Across
  workspaces there are no invariants, only actions that ask.
- **Preconditions.** `require cond else "message"` in an action body is for preconditions that
  belong to the action, not the type.
- **Dependents.** Actions that replay and fail because an earlier one was rejected are presented
  as **one outcome** with it.

## 4. Effects outside the workspace: flows

Actions are pure. Anything leaving the workspace is a step in a `flow`: a foreign Tilo action
(`bank.transfer(...)`) or an `outside action` (`payments.charge(...)`, sending mail).

- **A call step runs only once the steps before it are final.** Money never moves for a booking
  that is later rejected.
- **Every call carries its invocation id as its idempotency key.**
  - **Foreign Tilo actions:** any device may deliver the call. The receiving workspace's host
    orders it, deduplicates it by id, checks its own invariants and the caller's authority (a
    grant such as a spending mandate), and returns a result.
  - **Outside systems with idempotency keys** (most payment APIs) behave the same. The adapter
    maps the id to the key and the response to the result. It runs where its credentials live,
    on the host or the owner's device.
  - **Outside systems without idempotency** (SMTP, many legacy APIs) get one executor, through
    the outbox. It records "sending" durably before the call. A crash mid-call makes the
    invocation **outcome unknown**: shown to a person, never silently retried.
- **Put an invariant where the thing it protects lives.** A trip budget that is real money is a
  bank sub-account or a mandate with a limit. It is not a second invariant in the trip competing
  with the bank's balance.

## 5. What people see

The platform tracks two facts for every action, and views decide how prominent they are:

- **Final:** provisional, then final or rejected. The rejection carries its message and its
  dependents.
- **Delivered:** this device, then the host, then the people it concerns, then seen by them.
  - Chat shows this on every message.
  - A note shows it only when it is stale and someone else is looking.
  - A booking shows finality prominently.

When the host is reachable, nearly every action is committed within one round trip and no state
is ever shown. The UX has three exceptional states to design, each explicitly, starting from real
apps (chat, signup, payment):

- pending offline;
- rejected;
- outcome unknown.

**Approval** follows from an action's danger level and the viewer's authority, decided centrally.
Emptying the trash asks even someone allowed to do it. An action may supply a **confirmation
view**: an ordinary view selecting the values that make the choice reasonable. The person acting
sees it as a modal; others see the pending action with the same view.

## 6. The cases

| Case | The developer writes | Outcome |
|---|---|---|
| Tick a todo | `check(t, done: Bool = not t.done)` | Converges, offline, never rejected |
| Like a post | `likes.add(1)` | All count |
| Chat message | `messages.append(..)` | Never rejected; delivery shown |
| Move a folder | `n.parent = into` on a `Tree` | A cycle is rejected with its reason |
| Workshop signup | `invariant attendees.count <= seats` | The order decides the last seat; the loser is told |
| Assignment deadline | `hosted type Course`, `invariant submitted_at <= due` | The host's clock decides |
| Selling to strangers | `hosted type Listing`, `invariant once buyer` | The host decides |
| Removing a member | nothing (platform, hosted) | Later edits by that member are refused |
| Paying through a Tilo bank | a `flow` calling `bank.transfer` | Runs once final; the bank orders and deduplicates |
| Paying through Stripe | a `flow` calling `payments.charge` | The same; key = invocation id |

## 7. Consequences for Tilo

- **An action log becomes the source of truth.** It sits above Automerge, which keeps storing the
  commuting kinds (text, counters, sets). Widgets that write the CRDT directly become actions:
  every widget event is already an action reference or a binding in `components.md`.
- **The host's commit role** reuses the governance owner's ordered admission.
- **The flow executor and outcome unknown** reuse the outbox and the native owner's existing
  unknown-outcome state.

## 8. Open

- The design of the three exceptional UX states, per app.
- **Checkpoint cadence and retention of old package versions** for replay.
- **Whether `hosted` should also be selectable per action,** not only per type. Per type is
  simpler, and every example so far fits it.
