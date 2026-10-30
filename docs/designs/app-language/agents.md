# Agents writing tilolang

Agents are excellent at shell one-liners: one `rg … | xargs sed …` does what would take a person
ten minutes. A tilolang program on the door must be at least as terse, and better: correct where
the shell guesses, with authorship, history, permissions and undo. Today's `tilo_*` tools already
work even with cheap models; tilolang has to beat them, measured (§9).

Everything here is ordinary tilolang, not an agent dialect: `grep`, the chaining functions,
assignments that compile to actions, and the typed decisions in §5 are available to every
program (apps, shortcuts, flows, views). Agents just use them most. Calls that use a model
declare it like any effect (`uses model`, like `uses web`), and their answers are recorded.

## 1. One tool, programs, sessions

- The door (Tilo's MCP server, `docs/runtime-mcp.md`) has a tool `tilo` that takes a
  **program**. Everything sent and received is tilolang, and output pastes back as input.
- Only what the task may use is mounted: installed apps' types, views and the actions the task
  is allowed. Anything else is a compile error that names what is allowed, so nothing denied is
  ever attempted.
- A session keeps its `let` values between calls.
- A program runs sandboxed as the task, in a tilolang host whose author is that task.
- **A task is many programs.** Exploring, dead ends, failures, asks, intermediate results: each
  call is one step; the task is the session of steps, with its trail and pending asks. You can
  undo one step or the whole task.
- The existing tools (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`) stay as one-call forms.

### How a program runs

1. **Who:** the task. The program's code author is the agent task, acting for its person; what
   it may do is the lesser of both (authority.md). It never runs as an app or borrows an app's
   authorship.
2. **Where:** one tilolang host per task, with the task as author (ui-host.md). The door starts
   it on the task's first `tilo` call; it lives as long as the task, holds the session's `let`s,
   opens no windows and owns no records.
3. **What it names:** every installed app's published types and actions, from the one schema
   (action-system.md step 4), as far as the task may use them.
4. **How it reads:** as any reader outside an app: `find` and `grep` through the system index,
   fields through the platform's read path, results as catalogue references. Sources from
   different apps combine (`docs + notes`).
5. **How it changes:** only by invoking actions. An assignment compiles to the resource's field
   edit, a method to its type's declared action; each is a broker invocation, a child of the
   task, decided at the action's own node (action-system.md step 3). A program never writes a
   record directly.
6. **Steps:** one call is one run group for undo; atomic within one origin, flow steps across
   origins (convergence.md). A preview is a provisional run of those invocations; `confirm`
   sends the same program again.

### Reading records outside their app

A room's records are sealed per app: today only a connection holding that app's launch custody
opens them. The task host is not an app, so it gets its own admission instead of borrowing one:

- **Admission.** The door serves the task as *the agent acting for the person* who admitted
  it (`Caller::local_agent(agent).acting_for(person)`; `BrokerActor` agent for person, never
  session-delegated, so no preset widens it). The task host's only connection is its program
  connection to the door: it holds no app's route, no broker connection and publishes nothing.
- **Reads, per record.** The task host asks the door for each record it reads, once per
  program (`{"record": {app, record}}`; the person's own record for an app with
  `records: .own`). The broker decides it at the record's node with that actor, the lesser of
  the agent's and the person's grants (`RecordReads`, `record_read.rs`), and the room's
  document owner captures it under the person's device certificate, whose own check is the
  person's read. The capture decides the agent's read again before every chunk it hands out.
  The task host gets the record's committed content, read-only: the record channel's keys
  never leave the broker. It reads that content with the app's own package, the reads an app's
  host answers (`task_records.rs`), and builds the index `find` and `grep` search from records
  that passed the check. A revoked grant or a removed member cuts the next read and one under
  way. No app runs for a read, so whether its window is open never matters.
- **Changes, never writes.** The task host gets no write custody. It plans the action from the
  app's package over the record it read, for the preview only, and sends the invocation
  (action, arguments, the record it changes, the planned writes) to the door. The broker
  decides it at the action's node for the agent acting for the person (`RecordChanges`): an
  Ask answers that it asks the person first, a Deny never leaves the broker. The record's origin applies it:
  the host of the app's author in that workspace, handed the app's collaboration connection
  for that record as an instance's is, holds the record and the app's package with no
  instance and no window (`hosted::Applier`). It runs the action's code against the record as
  it is then, so requirements and invariants are decided on what others wrote since the
  task's read, applies what it wrote as the invoker's step and hands it to the room; a
  requirement that now fails refuses with its reason, and a result that differs from the
  preview is applied and reported as differing. Undo goes the same way, by the app's id and
  record. An app's own instance is never asked to act for the program.
- **Native apps.** Their types and views come from the published catalogue the same way. Their
  action code is the app's Rust, so the origin routes the invocation to whatever executes it: a
  window of the app when one is open, else an instance of the app with no window. For an action
  that shows nothing the broker asks the host for one (`LauncherServices::windowless`) instead of
  launching the app; the session mounts the app's own component on the room's record in its own
  process, never drawn, connected as the app and marked windowless (`shell/windowless.rs`), and
  the broker delivers to it. Tasks runs this way, so asking an agent and a run's reports to its
  issue open no Tasks window. An `open` never goes to a windowless instance: it launches a
  window.

## 2. What the agent knows from the start

The session's first message says where it is: the workspace, who it acts for, the task's limits,
and what the person was pointing at if they pointed (canvas.md). There is no `look` call. Words
for the obvious things: `me`, `this` (what the person pointed at), `here` (what `this` is in),
and times like `today`, `-7d`.

It also lists one line per app the task can use; `describe` gives the detail on demand.

## 3. Querying

Three familiar shapes:

- **`grep(regex, in: …)`** over the text of any resources, with match groups. It returns
  matches with refs and anchors (`d12 ¶4: …the Tide app…`), counts first. It runs on the system
  index and confirms each match against the real text. Its filters are facts the system knows
  and a shell can only guess: block types (`not: .quote`), `changed: -14d` (from the log),
  `mentions: me`, and `.written_by` on each match (who wrote that run of text).
- **`find("…")`**: system search with the palette's query string (search.md), for broad
  lookups (`find("lisbon justin")`), or typed (`find(Photo, people: all(me, justin))`).
- **Chaining with key paths** for everything after: `filter`, `map`, `sort_by`, `count_by`,
  `group`, `take`, `each`, `single` (exactly one, or an error listing the candidates).

Sources are values: other apps' roots combine (`chat.messages + mail.messages + notes`), and
relationships are system fields (`replies`, backlinks, `created.by`, `written_by`).

Every result says what it couldn't cover: "38 match; 54 have no place; 12 have unnamed faces".
An agent can see when an answer is incomplete and widen the search in the same program.

```
grep(/\bTide\b/, in: docs + notes).count_by(.doc)
grep(/\bTide\b/, in: docs + notes, not: .quote).replace("Tilo")

grep(/^- \[ \] (.*launch.*)/i, in: docs + notes, changed: -14d)
    .each(t -> tasks.add(launch, title: t.group(1), assignee: t.written_by, link: t))
```

## 4. Changing things

- Assignments and methods read like ordinary code (`n.title = …`, `t.done = true`,
  `hits.replace("Tilo")`) and compile to the type's real actions: field edits, text splices,
  declared actions. Permissions, attribution and undo apply; nothing bypasses them.
- **One call is one step.** Changes to workspace resources in one call commit together, at one
  origin, all or nothing. Calls to outside origins (a mail server, YouTube) can't be atomic with
  the workspace; they run as flow steps in order (convergence.md, "Flows").
- **Preview is a property of the call.** When a program would change many resources or destroy
  something, the door answers with a preview instead of applying it; the agent sends the same
  program again with `confirm`.
- Drafts and other undoable changes in the workspace just run. Anything that reaches outside or
  is ask-first parks as the person's request, with an editable draft.

## 5. Judgment and bulk

The agent is the model: it reads results and writes the next program itself, across rounds,
with its own titles, summaries and replies. Model calls inside a program are only for volume the
agent shouldn't read itself, as typed decisions on small, cheap decision models through the model
gateway:

```
enum Topic { billing, bug, other }
mail.messages.filter(.received > -30d).classify(as: Topic).count_by(.label)
asks.score("how urgent is this for me").sort_by(.score, desc)
asks.where("asks me to do something", above: 0.8)
```

`classify` returns values of a real enum. The answers are recorded with the call, so the step
stays reproducible and undoable.

## 6. Reading and describing

- `read(r, matching:, around:)` returns text with anchors; big content is paged and states its
  size first. Reading never marks anything read for the person.
- `describe` works on an app, type, action, field or ref: declarations as tilolang, what this
  task may do, and an example value from the app's scenarios.
- `print(xs, fields: [.a, .b])` prints rows; without `fields:`, a type's declared filter fields.
- Refs (`t91`, `m3`) and anchors (`m3 ¶2.2`) start every output line. The compiler accepts only
  refs that appeared in the session's output, so none can be invented.

## 7. Errors that teach

Errors say what's wrong and what's valid ("Thread has no filter `sender`; did you mean
`from`?"). Tolerance comes from these errors, not from a parser that silently accepts variants.
Naming conventions (singular types, verb actions, adjective booleans, past-participle times)
make first guesses mostly right.

## 8. Answering and the trail

An answer composes views (agent-ux.md); the door checks it before the person sees it: quotes
resolve, suggested actions are allowed, it fits. After a task, its result is checked against
the request: on by default for small-model profiles, which skip or get parts of long requests
wrong most often. `history task` lists the steps; `undo task` reverses them.

## 9. Evals

Reusing what exists (`scripts/perf/agent_task.py` with its fake model, agent-participant's
`campaign_bench` and `campaign_replay`, `crates/dev/ai-bench`, the `instruction_mode_eval`
scorer): a seeded simulated workspace (chat, mail, notes, files, tasks), about 30 tasks with
checkable outcomes, cheap and strong models, and four interfaces: today's `tilo_*` tools,
tilolang chaining, tilolang with pipe sugar (`|` as the same chain), and TypeScript against the
same API. Measured: first-try errors, tokens, calls, correctness. The syntax follows the results.
