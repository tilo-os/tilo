# Agents writing tilolang

Agents are excellent at shell one-liners: one `rg … | xargs sed …` does what would take a person
ten minutes. A tilolang program on the door must be at least as terse, and better: correct where
the shell guesses, with authorship, history, permissions and undo. Today's `tilo_*` tools already
work even with cheap models; tilolang has to beat them, measured (§9).

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
