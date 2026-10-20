# Tilo App Language — initial spec (draft 0, 2026-10-20)

Status: a design draft for discussion. Nothing in it is implemented. Legacy is ignored: every app
would be rewritten in this language. The compositor, UI framework, sync and storage stay in Rust;
in this spec they are called "the platform".

## 1. Goals

- **One language** for apps, for the customizations people and agents make, for shortcuts, and for
  the blocks agents put in their answers.
- **The model is the language.** Resources, actions and views are the language's own constructs,
  not a library layered on top.
- **Fast to write and to run.** An agent writes code in conversation and it runs within
  milliseconds. Installed apps run close to native speed. The same code runs in the browser.
- **Hard to misuse.** The types are sound. Code that changes something is distinguished from code
  that doesn't. Authority is tied to whoever wrote the code.
- **Nobody sees code unless they ask for it.** People see views, and they see actions as they
  happen or as suggestions.

Out of scope: the compositor, the render core, codecs, the GPU. The platform provides these to the
language as capabilities.

## 2. Core model

### 2.1 Types are resources

Every value of a declared type can become a resource: state with identity, history, permissions
and sync. It becomes a resource when it is attached to something persistent, such as a workspace
or another resource's field. Until then it is a plain value. That is persistence by reachability.

A resource is **only state**. Actions and views are associated with a type but are not part of its
state.

```
type Album {
  title:  Text   merge register
  tracks: Collection<Track>
  plays:  Int    merge counter
}
```

### 2.2 Merge semantics live in the field type

Each field's type says how concurrent edits combine. A merge type also defines the operations the
field allows, so a mutation records what the writer meant rather than the resulting value.

The primitive set:

| Primitive | Concurrent edits |
|---|---|
| `register` | Last writer wins. Conflicting values stay readable. |
| `counter` | Increments add up. Its operations are `+=` and `-=`. |
| `Text`, `RichText` | Splices, marks and blocks merge. |
| `Set<T>` | Per element. |
| `Map<K, V>` | Per key, with each value using its own merge. |
| `Collection<T>` | Stable item ids and order keys. A move keeps the item's identity. |
| `Grid<T>` | Per cell, with stable row and column ids. |
| `Outbox<T>` | Entries are settled by the device that owns them. |

- Records and collections nest, so merge types compose.
- **Custom merges** are pure, deterministic resolver functions over the concurrent values. They
  are versioned together with their type, since every peer has to compute the same result.
- The compiler rejects mutations that would lose intent, for example `+=` on a `register`. A
  read-then-write has to be written out explicitly.

### 2.3 References

- `@kind:id` is a **live** reference to the current state. This is the normal case.
- `@kind:id@version` is a **pinned** reference to a fixed version, and it is displayed as pinned.

A reference is not a capability. What a viewer can do through it is decided by their permissions
(§4).

### 2.4 Identity

Every declaration has a stable id that is separate from its name: types, fields, actions, views,
anchors. Storage, references, grants and compatibility checks all use ids. Names are metadata.
The AST is canonical (§8).

## 3. Code

### 3.1 Functions, actions, flows, views

| Form | May do | Notes |
|---|---|---|
| `fn` | Pure computation | Can't mutate anything. Calling it has no effects. |
| `action` | Read and mutate resources | A **synchronous transaction**. It may not wait on anything. |
| `flow` | Call actions, wait, branch | Durable and resumable. This is how anything asynchronous is done. |
| `view` | Read resources and view state, return UI | Pure. Inside a view, an action application is a *reference*, never a call. |

Inside an action, code is ordinary sequential code with local variables and loops. Only the
mutations it produces are recorded.

```
action play(a: Album) {
  a.plays += 1
  player.queue(a.tracks)
}
```

### 3.2 Action semantics

- An action runs as **one atomic change** with **one history entry** that names its author and the
  principal it ran on behalf of. It can be undone through the inverse defined by each merge type.
- Effect classes are derived by the compiler, never declared by hand:
  - **Read:** reads only.
  - **Undoable:** CRDT mutations.
  - **Outside:** irreversible effects that leave the system, such as sending mail, network calls or
    payments. Each runs through an `Outbox` and has a `compensation` action where one is possible.
- An action may name a **proposal view** over its arguments (§5.4).
- An action may override its **description**, the verb and delta text used in activity lists. By
  default the description is derived from the contract and the schema diff.

### 3.3 Flows

A flow is a sequence of actions with suspension points between them: waiting for approval, waiting
for an Outside result, a timer, a trigger.

- **Deterministic.** There is no ambient time, randomness or I/O. Every source of nondeterminism,
  including model calls, is an action. Its result is journaled.
- **Durable.** Resuming a flow replays its journal. A flow that waits days for an approval is just
  a journal on disk.
- Shortcuts and agent scripts are flows. A flow made only of actions can itself be exposed as an
  action, with its effect class derived from the actions it contains.

### 3.4 Views

```
view album_page(a: Album) {
  column {
    heading(a.title)
    list(a.tracks) { t -> track_row(t) }
    button("Play", on: play(a)) #play
  }
}
```

- The platform evaluates views with fine-grained reactivity. A view is built once, and only the
  parts that read changed paths update.
- `#name` declares an **anchor**: a stable, documented point where other code may patch the view.
- Views receive **provenance** (who changed a field or item, when, and through which invocation)
  and **presence** (who is where). Controls bound directly to fields render both automatically
  (§5.5).
- How view state is shared is open (§10.1).

## 4. Authority

### 4.1 Principals

The principals are people, agents (in v1, a delegate of a person per task), apps (identified by
code hash) and code authors in general.

### 4.2 Answers and defaults

- Every grant answers **Allow**, **Ask** or **Deny**. It applies to a node of the tree workspace ›
  app › type › resource › item › field › action.
- Defaults for a person's own gestures and for their agent: Read and Undoable are allowed, Outside
  asks.
- Workspaces pin app versions (§7).

### 4.3 Authority follows authorship

1. **Every action reference carries the identity of the code that wrote the application
   expression.** The runtime enforces this (§6.2). Passing your reference into someone else's view
   doesn't change its author.
2. **Effective authority = min(viewer, author).** It is evaluated **when the action is invoked**,
   not when the code was written. If the author loses a permission later, every button they wrote
   loses it at once.
3. **Own vs foreign.** Code may invoke its own actions freely: the actions declared by the same app
   or module, on resources of its own types. Opening an app is enough for that, and nothing has to
   be installed. Invoking anything foreign needs a grant: another app's actions, other resources,
   any Outside effect.
4. **Code from strangers has no grants**, so all its foreign invocations ask.
5. **Your agent's code** is limited by the agent's grants. A button in a view the agent made can
   let you do at most what the agent itself may do. Anything beyond that asks.
6. **Nesting takes the minimum along the whole chain:** viewer, then each author in turn.
7. **Pinning or adopting code** moves its authority from the expiring task to your trust in that
   exact version, identified by hash. A changed version needs consent again for any capabilities it
   adds.
8. **The platform renders confirmations, never the view.** Whenever an invocation falls to Ask, the
   prompt appears in trusted chrome and shows the action's own representation: its proposal view or
   its description. The view's own label is not used.
9. **Disclosure.** Hovering or focusing any control bound to an action shows the real action in
   trusted chrome.
10. **Attribution.** History records the whole chain, for example "you, via Alex's view".

### 4.4 Static capability sets

For any module, the compiler computes the set of actions it can reach. Granting, adopting and
updating are decided against that set, shown in a form people can read. Nobody has to read the
code to make these decisions.

## 5. Agents

### 5.1 Agent answers contain blocks

An agent's answer is a message: rich text interleaved with **blocks**. Each block is an expression
in this language.

- A block whose value is a **view** renders inline.
- A block whose value is an **invocation** renders through its invocation view (§5.5). It has
  either happened already (Allowed) or is waiting as a suggestion (Ask).
- A block whose value is **data** renders through its type's default view.
- References in blocks are live unless they are pinned.

Agents write blocks as fenced code in the canonical text projection:

````
Good morning. Here is what matters today:

```tilo
good_morning(
  @music.album:01J8ZK4T,
  [@feeds.feed:01H2QM7C, @feeds.feed:01H9XR2B],
  [(@mail.message:01JC3V8N, "Forge access request, needs your OK")],
)
```
````

### 5.2 Codemode

Between answers, the agent runs **flows** whose results return to the model rather than to the
user. This replaces tool calls.

- A flow's value goes back to the model as the **model projection** of its type: a compact,
  structured representation that is derived from the schema by default and can be overridden per
  type.
- Large values stay with the runtime as references. The model sees ids and summaries, never dumps.
- **Discovery** is a query over typed signatures, for example "actions on `Album`" or "views of
  `Message`". The signatures are the documentation.
- Reads leave a quiet trace in the agent's activity, so the user can see what an answer was based
  on.

### 5.3 Where agent code runs

- **Views** evaluate in each viewer's runtime, with the viewer's permissions.
- **Agent flows** run in the runtime that hosts the task. That can be any of the person's trusted
  runtimes: their devices or Tilo's cloud.
- **Outside actions** run where the capability lives. A suggestion may be approved on one device
  and executed on another.

### 5.4 Suggestions

A suggested action is an action applied to arguments. The arguments form a resource that lives only
while the suggestion does, owned by the task.

- The suggestion renders through the action's **proposal view** over that argument resource. For
  example, Mail's own compose view over the draft. Underneath, trusted chrome shows a bar with
  Confirm, Edit, Decline and Always allow.
- Editing the suggestion edits the argument resource.
- Confirming invokes the action with the current arguments, and the confirmation is bound to them.
- If an action has no proposal view, the platform renders a form from the parameter schema.
- While waiting, the flow is suspended (§3.3).

### 5.5 How actions are represented

| Situation | Representation |
|---|---|
| You are looking at the resource | Presence (cursor, selection, focus) and provenance highlights that fade. Moves are animated, since items keep their identity. |
| You are somewhere else | An invocation view made of the target's item view, the verb and the delta. Invocations are grouped by task step ("moved 40 files to Archive/"), and clicking one opens the resource at the change. |
| Waiting | The suggestion (§5.4), shown inline in the answer and in the task list. |
| Watching live | An explicit **follow** mode. Your view jumps to the other party's latest focus without chasing every action. |
| Afterwards | Replay a task at human speed through its recorded changes, in the real views. |

All of this applies equally to people. Agents are collaborators that are fast and sometimes need
permission.

## 6. Execution

### 6.1 Target

- The **only** compilation target is wasm. Natively it runs on wasmtime: a baseline compiler gives
  instant start, and Cranelift compiles code that runs hot or is adopted. In the browser it runs in
  the browser's own wasm engine.
- Compiled modules are cached by content hash. An agent's block compiles in milliseconds.

### 6.2 Trust boundary

- **Memory:** the wasm sandbox.
- **Authority:** the host API.
  - Resource references, action references and capabilities are **unforgeable host handles**.
  - The host records which module, and so which author, created each handle.
  - A module that bypasses the compiler still can't forge authority.
- **The compiler is not part of the trusted base.** It is responsible for types, effect classes,
  capability sets and errors, not for security.

### 6.3 Instances and limits

- Each author's module gets its own wasm instance inside the trusted runtime process. Starting one
  costs microseconds, and no app needs its own process.
- Epoch or fuel limits interrupt runaway code. Memory is limited per instance.

### 6.4 State

- Resources are CRDT documents owned by the host. Modules read them through handles and mutate them
  through actions.
- Module heaps hold only ordinary transient state, managed with WasmGC or with reference counting
  plus arenas per invocation.
- The host provides bulk APIs, so going through collections doesn't become one host call per item.

## 7. Versions and compatibility

- Every change records the content hash of the package that produced it.
- **Workspaces pin app versions** in v1. Updating is a decision for the whole workspace.
- On update, the compiler re-checks everything that depends on the changed surface: views,
  flows, pinned answers, other apps. Breakages come back as a precise to-do list, which agents
  fix.
- Schema changes go through versioned migrations. A migration never deletes user data or changes
  an id, and items that can't be read are quarantined, never dropped.
- Bidirectional lenses between coexisting versions are deferred.

## 8. Syntax and projections

- The **AST is canonical** and every node carries an id.
- There is **one fixed text projection**. Agents write it and errors refer to it.
- Projections customized per person come later and change presentation only.
- In the projection, references display their target's current label, for example
  `@music.album:01J8ZK4T ⟨Blue⟩`. The label is display only and is not stored.

## 9. Examples

### Counter

```
type Counter { value: Int merge counter }

action increment(c: Counter) { c.value += 1 }

view counter(c: Counter) {
  column {
    text(c.value)
    button("+1", on: increment(c)) #plus
  }
}
```

### An outside action with a proposal view

```
type Draft { to: Set<Address>, subject: Text, body: RichText }

action send(d: Draft) proposal compose(d) {   // Outside, derived from outbox.push
  outbox.push(d)
}
```

### A shortcut as a flow, exposed as an action

```
flow morning_cleanup(inbox: Mailbox) {
  let news = mail.search(inbox, unread: true)
               .filter(m -> models.classify(m, "newsletter"))
  for m in news { mail.archive(m) }        // foreign actions: needs grants
  return news.len()
}
```

## 10. Open questions

1. **Sharing view state:** scroll position, input fields, reading together. This is next.
2. **Branches.** Running on a fork and approving a whole change set, for agents and for previews.
   The main tree is enough for v1.
3. **Lenses** so versions can coexist (§7).
4. **Privacy rules for remote models.** v1 is local-model-first.
5. **Representing a change set inside views**, for when branches arrive: a proposed mode for views,
   or a generic diff derived from the schema.
6. **Constraints in grants**, such as "only reply to people already in this thread". Deciding how
   expressive these may get.
