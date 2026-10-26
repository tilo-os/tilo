# Agents: a workspace an agent can use quickly

An agent working in a Tilo workspace should feel the way a good engineer feels in a familiar
shell:
- it knows where it is;
- it guesses names right;
- it finds things in one step;
- it sees what it may do;
- it checks its work before a person sees it.

This page is about that interface. The model underneath it (types as resources, actions,
authority following authorship, agents as tasks) is in `spec.md`. Search is in `search.md`, and
answers that embed views are in `components.md`.

The interface is designed from the agent's side. Each rule below removes a step that an agent
otherwise spends tokens, calls or guesses on.

## 1. One language, one tool

**Everything an agent says is tilolang, and everything it is told can be pasted back.**

- **Queries, actions, reads and answers** are tilolang expressions and statements. There is no
  separate JSON schema per tool, query DSL or answer format.
- **Types and actions describe themselves as source.** `describe` prints a type or an action as
  its tilolang declaration with its doc comments, the way the author wrote it. Agents read code
  faster and more reliably than they read schemas.
- **Output is input.** Every ref, filter and literal in an answer is valid in the next
  request.

The door is one MCP tool, `tilo`, taking a line or a block. The same language is the `tilo`
command in a terminal. The existing path tools (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`)
become spellings of the same requests and keep working.

## 2. Orientation

```
> look
workspace  Acme · you: Xaver · 3 people · synced
focus      Mail › Inbox › t91 "Incorporation & SAFE: next steps"        (this)
selection  m3 ¶2.2
open       Mail, Todo, Notes › "Board meeting"
apps       mail todo notes chat calendar files
task       "check my mail" · may: read all, write drafts and todos · asks: send, delete
```

`look` answers four questions in one call:
- where am I;
- what is the person looking at;
- what is around;
- what may I do.

Without arguments it shows the workspace. `look t91` does the same for one resource: its
parents, its children by kind with counts, its actions, and who else is on it.

**Words for the obvious things:**

| Word | Means |
|---|---|
| `this` | what the person is looking at: their focused resource |
| `selection` | what they selected, with its anchor |
| `me` | the person the task acts for |
| `here` | the resource `this` is inside: the thread, notebook or list |
| `today`, `-7d`, `last week` | times relative to now, in the person's time zone |

"What does this say?" becomes `read this` with no searching. "Add that to my list" becomes
`do todo.add(here, selection)`.

## 3. Refs

- **Every resource has a short ref,** unique within the workspace, like a git short hash:
  `t91`, `m3`, `f7`. The prefix comes from the type's initial and grows only when needed.
- **A full path is also accepted:** `mail/Thread/t91`, and a path by title,
  `notes/"Board meeting"`.
- **Every line of output starts with the ref of what it shows,** so an agent never has to
  look one up again.
- **Anchors** point inside a resource: `m3 ¶2.2` is a paragraph, `f7 p.4` a page, `doc §3` a
  heading, `t91 m3..m5` a range of messages. They are what `quote(...)` cites.
- **Addresses are stable.** They don't change when a title does, and a moved resource keeps
  its ref.

## 4. Finding

`find` is the query from `search.md`, written with the same tokens people type in the palette:

```
> find Thread from:@ycombinator.com sort:-last
3 threads
  t91  Incorporation & SAFE: next steps  Jane Doe    Tue 14:02  unread  3 msgs  Legal
  t40  Welcome to YC: legal checklist     YC Legal    Sep 2             5 msgs
  t12  Batch kickoff                      YC          Aug 28            12 msgs

> find Todo "83(b)"
0 todos

> find from:jane received:-30d
12 results: 9 messages, 2 notes, 1 event
  m3   Re: Incorporation & SAFE …
```

- **Columns are the type's declared filter fields:** what the app said matters about the type.
  The row's title comes first. Counts come before rows.
- **Pages** default to 10 rows. `more` continues the last result, and `find … | count` answers
  without rows.
- **Tokens:**
  - mail addresses and `@domain` on mail-address fields, and `person:jane` for an explicit
    expansion through Jane's associations (`search.md` §2.2.1);
  - `me`;
  - `is:unread`;
  - `-7d` and `today`;
  - `~"fuzzy text"`;
  - `in:t91` for scope;
  - `sort:-received`.

  The full expression form (`find(Thread, where: {…})`) is the same query, for when it's built
  in code.
- **Pipes over results:** `| count`, `| first`, `| fields subject from`, `| group from`. They
  are tilolang collection functions, written after the query.

## 5. Reading

```
> read t91 new
m3  Jane Doe <jane@ycombinator.com>  Tue 14:02  unread
  ¶1   Hi both, great progress this week.
  ¶2   Next steps:
  ¶2.1 Sign the stock purchase agreement (sent via DocuSign).
  ¶2.2 File your 83(b) election within 30 days of the purchase date (Nov 14).
  ¶2.3 Send us the cap table export from Carta.
  ¶2.4 Open a bank account in the company's name.
m4  you  Thu 09:13
  ¶1   Signed the SPA, see attached.  [f7 SPA-signed.pdf]
```

- **Text with anchors.** Every block carries its anchor, so citing is free. Rich content
  (tables, headings, attachments) is marked up as compact markdown, as `tilo_read` does today.
- **Ranges and the rest:** `read m3 ¶2..¶3`, `read f7 p.4`, and `read t91 new`, meaning what
  the person hasn't seen.
- **Big content is paged.** A read states its size first ("f7: 23 pages, 9k words; read f7 p.1
  …"). It never dumps more than a page.
- **Reading is not seeing.** A task's reads don't mark anything read for the person.

## 6. Learning what is possible

```
> describe Thread
/// A conversation by mail: its messages, in the order they arrived.
type Thread {
    subject: Text (title)
    messages: Collection<Message>
    labels: Set<Label>
    search { filter unread  filter participants  filter last  filter labels  sort last newest }
}
actions on Thread                          this task
  archive(thread)                          may     /// Moves the thread out of the inbox.
  label(thread, label: Label)              may
  draft_reply(thread, body: Text) -> Draft may
  send(draft: Draft)                       asks    /// Sends a draft. Leaves the workspace.
  delete(thread)                           no

> describe mail
app mail "Mail" · types: Mailbox Thread Message Draft Label · start: look mail
```

- **`describe` works on anything:** an app, a type, an action, a field, or a ref, which
  describes that resource's type with this resource's values.
- **Permission is shown before acting,** per action, for this task.
- **Doc comments are the documentation.** The compiler requires one on every public type and
  action, and they are written for the people and agents who will call it.

## 7. Acting

```
> do archive(find Thread label:Newsletters received:<-7d)
preview: archives 41 threads (Newsletters, older than 7 days); undoable as one
  t88 Weekly digest · t87 Product Hunt · t85 … (38 more)
confirm with: do!

> do!
done: 41 threads archived · undo: undo task
```

- **Actions take queries.** An action declared on one resource applies to each result of a
  query, as one invocation with one undo, in one history entry.
- **Preview first, wherever the effect is larger than one resource or is destructive.** It
  says what changes, how many, whether it's undoable and whether the person will be asked.
  `do!` runs the last preview. A single, undoable action on one resource runs at once.
- **Defaults are filled in and shown.** An invocation-time default (`convergence.md` §1.1)
  is recorded, and printed as part of the result.
- **Asks** park the action. The person sees the pending action with its confirmation view. The
  agent sees `pending: asks Xaver (send)` and can wait or continue.

## 8. Errors that teach

Every failure has compiler quality: what was wrong, where, and the valid alternatives.

```
> find Thread sender:@ycombinator.com
error: Thread has no filter `sender`; did you mean `from`?
  filters: from to participants last unread labels has_attachment

> do archive(m3)
error: archive takes a Thread; m3 is a Message in t91
  did you mean: do archive(t91)

> do send(d4)
asks: send leaves the workspace; Xaver confirms it (pending i22)
```

**Guessing must mostly work.** The compiler enforces conventions so an agent's first guess is
usually right:
- types are singular nouns;
- actions are verbs;
- booleans are adjectives or states (`unread`, `done`, `pinned`);
- times are past participles (`received`, `created`, `due`);
- collections are plurals.

The conventions are in the style guide, and the compiler warns on violations.

## 9. Answering

An answer is a tilolang block (see the example in `components.md` and the chat design). The
door compiles it **on submit** and returns:
- **diagnostics,** exactly as a compiler would;
- **a text rendering of what the person will see,** the same compact form as `read`: rows,
  their icons and their buttons;
- **checks:**
  - every `quote(...)` resolves to the cited text;
  - every suggested action is one this task may offer;
  - nothing exceeds the answer's size.

An agent therefore checks its own work before the person sees it, the way it runs tests before
reporting.

## 10. Its own trail

- `history task` lists what this task did, with refs.
- `undo task` undoes it all, and `undo i22` undoes one action.
- `watch <query>` makes a standing task: "tell me when the school writes" is
  `watch Thread from:@school.example`. It is a saved search that notifies, owned by the person
  and visible in their settings.

## 11. Budgets

Things are only quick when replies are short and fast:
- every reply states counts before content and fits a page by default;
- `look`, `find` and `describe` answer within the palette's budget (8 ms locally,
  `search.md` §4);
- the door never makes an agent poll: pending actions answer when they settle, as `tilo_do`
  does today.

## 12. What changes

1. **The door:**
   - the `tilo` tool and CLI parse tilolang requests;
   - the path tools are kept as spellings;
   - `look`, `describe`, short refs and anchors are added.
2. **Compiler:**
   - doc comments required on public items;
   - naming-convention warnings;
   - requests and answers compiled against the workspace's installed packages;
   - "did you mean" for names, filters, actions and argument types.
3. **Search:** the query and tokens from `search.md`, shared with the palette.
4. **Runtime:**
   - query-wide action invocation, with one undo;
   - previews computed by a dry run against a copy of the state the action reads;
   - task history and undo.
5. **Acceptance:** an agent given only `look` completes the YC-legal task in at most 6 requests
   with no failed call. The same holds for "tidy my todo list" and "what changed in the board
   meeting notes since Monday". The transcripts are kept as tests.
