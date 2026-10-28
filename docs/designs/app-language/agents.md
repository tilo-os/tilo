# Agents writing tilolang

An agent in a Tilo workspace should work like a good engineer in a familiar shell: it knows where
it is, guesses names right, finds things in one step, sees what it may do, and checks its work
before a person sees it. Every rule below saves an agent a call, a guess or tokens.

## 1. One language, one tool

- Everything an agent sends and receives is tilolang: queries, reads, actions, answers. Output
  can be pasted back as input.
- The door (Tilo's MCP server, `docs/runtime-mcp.md`) gets one more tool, `tilo`, which takes a
  **program**. Every installed app's types, actions and views are available in it.
- Programs chain: query, read, loop and act in one call, like one big `grep`. A session keeps
  its `let` values between calls.
- A program runs sandboxed as the agent's task, in a tilolang host whose author is that task,
  so it can do no more than the task may. All its changes are one task step: one preview, one
  undo, and asks still ask.
- The existing tools (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`) stay as one-call forms.
- **A task is many programs.** Real work takes rounds: exploring, dead ends, failures, asks,
  intermediate results ("cool beaches nearby" searches maps and the web before composing an
  answer; "fix the video for launch day" runs for hours). Each `tilo` call is one step with its
  own preview and undo; the task is the session of steps, with its trail, pending asks, presence
  in the windows it touches, and an answer composed at the end or kept up to date as it goes.
  You can undo one step or the whole task.

```
let hits = find("launch video")
for n in hits.of(Note) { print(read(n, matching: "video", around: 1)) }
for m in find("video in:#launch received:-14d") { print(m.author, m.said) }
```

## 2. Where am I

Context usually arrives with the message: pointing at something and typing (canvas.md) sends
the view, resource and selection along. Otherwise:

```
> look
workspace  Acme · you: bob · 3 people · synced
focus      Mail › Inbox › t91 "Incorporation & SAFE: next steps"
selection  m3 ¶2.2
task       "check my mail" · may: read all, write drafts and todos · asks: send, delete
```

Words for the obvious things: `this` (what the person is looking at), `selection`, `here` (what
`this` is inside), `me`, and times like `today`, `-7d`, `last week`.

## 3. Refs and anchors

- Every resource has a short ref, like a git short hash (`t91`, `m3`); a path also works
  (`notes/"Board meeting"`). Refs don't change when titles do.
- Every output line starts with the ref of what it shows.
- Anchors point inside a resource: `m3 ¶2.2`, `f7 p.4`, `t91 m3..m5`. `quote(...)` cites them.

## 4. Finding

`find` is system search (search.md), with the same tokens people type:

```
> find Thread from:*@ycombinator.com sort:-last
3 threads
  t91  Incorporation & SAFE: next steps  Jane Doe  Tue 14:02  unread  Legal
  t40  Welcome to YC: legal checklist     YC Legal  Sep 2
  t12  Batch kickoff                      YC        Aug 28
```

- Columns are the type's declared filter fields. Counts come before rows; pages are 10 rows;
  `more` continues; `| count`, `| first`, `| group from` work on results.

## 5. Reading

```
> read t91 new
m3  Jane Doe  Tue 14:02  unread
  ¶2.1 Sign the stock purchase agreement (sent via DocuSign).
  ¶2.2 File your 83(b) election within 30 days of the purchase date (Nov 14).
```

- Every block has its anchor, so citing is free. Rich content is compact markdown.
- Big content is paged and states its size first.
- An agent reading something doesn't mark it read for the person.

## 6. Learning what's possible

```
> describe Thread
/// A conversation by mail.
type Thread { subject: Text (title)  messages: Collection<Message>  labels: Set<Label> }
actions on Thread                          this task
  archive(thread)                          may
  draft_reply(thread, body: Text) -> Draft may
  send(draft: Draft)                       asks
  delete(thread)                           no
```

- `describe` works on an app, type, action, field or ref, and shows what this task may do
  before it acts. Declarations print as tilolang, with an example value from the app's
  scenarios.
- `types ~"ideas"` finds where something can go without knowing the apps.
- Doc comments are the documentation; the compiler requires them on public types and actions.

## 7. Acting

```
> do archive(find Thread label:Newsletters received:<-7d)
preview: archives 41 threads; undoable as one
confirm with: do!
```

- Actions take queries: one invocation, one undo, one history entry.
- Anything larger than one resource, or destructive, previews first; `do!` runs the preview.
- Asks park the action: the person sees it with an editable draft, the agent sees
  `pending: asks bob (send)`.
- The agent never handles permissions, sync, formats, waiting (no polling) or presentation:
  the system does.

## 8. Errors that teach

```
> find Thread sender:@ycombinator.com
error: Thread has no filter `sender`; did you mean `from`?
```

Errors say what's wrong and what's valid. Naming conventions (singular types, verb actions,
adjective booleans, past-participle times) make first guesses mostly right; the compiler warns
on violations.

## 9. Answering

An answer is a tilolang block that composes views (agent-ux.md). The door compiles it on submit
and returns diagnostics, a text rendering of what the person will see, and checks: every quote
resolves, every suggested action is allowed, the answer fits its size.

## 10. Trail

`history task` lists what the task did; `undo task` undoes all of it, `undo i22` one action.
`watch <query>` is a standing task, a saved search that notifies.

## 11. The primer

The agent's system message is the whole manual; everything else is found with `look` and
`describe`. It's tested: the acceptance transcripts start from it alone. Replies are judged by
tokens and calls: one-line rows, refs instead of repeated titles, no echo.

## 12. Evals

- **Fluency:** the same 30 tasks in tilolang and in TypeScript against the same API, across
  models; compare first-try errors, tokens and calls. If tilolang loses, its syntax moves
  toward TypeScript.
- **Real tasks,** once Notes, Todo, Chat, Board and Mail run in tilolang: real agents in a
  seeded workspace, from the primer only. The format changes by what these show.
