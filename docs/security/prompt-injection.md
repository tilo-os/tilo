# Prompt injection: threat model and defaults

Status: **as built**, 2026-09. Entry point for how Tilo keeps untrusted content
from steering an agent, a skill or a shortcut's AI step. The controls it names
are in code with negative tests; where one is missing, the gap is listed with
what would close it.

## The problem, stated as a Tilo problem

An agent reads things its person did not write: a mail message, a web page, a
PDF someone mailed, a document a collaborator shared, a chat from a stranger.
Those bytes are *content*. The model that plans a task cannot tell content from
instructions by looking at it, so an attacker who controls one byte of what the
agent reads can try to write the agent's instructions.

Tilo's answer is not "make the model notice". It is that **instructions and
authority are different things**, and only the person's authority moves:

- The task's request, and the person's answers, are the only instructions an
  agent obeys.
- Everything an operation answers is data. It is labelled as such, and the
  label follows it into every plan.
- What a plan *does* is bounded by the delegation it was given and by the
  action policy, neither of which content can change.
- What a plan does that is irreversible, sends something outward, or follows
  from content that came from outside is put to the person first.

An injection that gets the model to "want" something therefore still has to
cross a confirmation or a permission boundary, and the person is shown that it
came from outside content.

## Adversaries

1. **A stranger who can get bytes in front of an agent**: mail, a shared
   document, a chat message, a web page, a search result, a PDF. They never
   have local code execution; they only write text a model will read.
2. **A collaborator with legitimate but limited access** who plants instructions
   in a record the agent reads, hoping the agent's task carries them out with
   wider authority than the collaborator's own.
3. **A page that answers a request's parameters** — search results, a fetched
   document, an API answer — inside the same turn as the person's instruction.
4. **A compromised app** whose operations answer attacker-chosen content under
   its own labels. It can mislabel; it cannot grant itself permission (that is
   the sandbox boundary, `docs/architecture/security.md`).
5. **A skill or shortcut author** who ships an instruction that makes a later
   task act on whatever it reads.

Out of scope here: a fully compromised kernel, and a model provider that
tampers with the answer (both are their own boundaries).

## Controls, and where they live

### 1. Untrusted content is declared by the operation, not guessed from text

An operation says whether its answer carries content from outside the person's
data with the `untrusted` effect row, which publishes
`tilo_actions::UNTRUSTED_CONTENT_TAG` (`crates/sdk/tilo-resource/src/effect.rs`,
`crates/collab/actions/src/contract.rs`). `ActionContract::carries_untrusted_content()`
is the single reading of it.

This is a declaration, not a heuristic: a read's trust does not depend on the
model's judgement, the text's wording or a classifier. The apps that publish
outside content today — the browser's page reads, web search, feeds, the git
client's checks — tag it at the operation.

### 2. The label follows the answer into every plan

The broker carries the tag onto an invocation's state
(`InvocationState::with_untrusted`, `crates/collab/actions/src/remote.rs`), and
so does the agent's planner: a task's own reads, a plan's reads while it is
being made, an item's answers in a coordinated task, and a record's snapshot
all keep the answer's trust with it (`crates/agent/agent-participant/src/planner.rs`,
`Answered`). Wherever a plan is shown an answer, an `untrusted` one carries the
note that it is data to reason about, never instructions to follow, and that a
step changing, sending or deleting something for it is put to the person to
confirm (`planner::untrusted_answer`, `model::with_results`).

The planner's instructions say the same thing to the model, in the same words
(`crates/agent/agent-participant/src/model/converse.rs`, `PLANNER_INSTRUCTIONS`),
and every content-taking tool describes its result as data.

### 3. The runtime's agent door labels it too

An agent reached over MCP (`tilo-runtime-mcp`) sees the same tag: an answer that
carries outside content is marked where it reaches the model, in a listing line
and in the value itself (`crates/runtime/tilo-runtime/src/mcp.rs`, `mcp_page.rs`).
This covers the scripting agent, not only Tilo's own planner.

### 4. Permissions bound what any plan can do

Authority is structural, not textual: an agent acts only within the delegation
it was given (which apps, which records) and within the workspace's file policy.
Content cannot widen it — a delegation is a signed grant from the person, and
the broker checks it per invocation. Undo captures every change an
`Undoable` operation makes, so the default damage from a successful injection
is bounded by what the person can revert.

### 5. Confirmation for the outward, the irreversible, and the tainted

Three things are put to the person regardless of the permission set:

- **Outside the person's machine** (`ActionKind::Outside`) on another device:
  the action is another party's to run.
- **Consequential and not undoable** — a send, a delete that Undo cannot bring
  back, a payment: `ActionContract::need()` asks, and `default_answer` in
  `crates/collab/action-policy/src/agent_answers.rs` says `Ask` for a
  consequential action, `Allow` only for a read or an undoable one.
- **Following from untrusted content**: a step whose argument derives from an
  answer labelled untrusted is confirmed like a sending one. The model is told
  this, and the confirmation shows the person the operation, its arguments and
  that its input came from outside.

An agent never confirms on its own word: an action that asks first is refused
by the agent's door and put to the person (`Participant::invoke`).

### 6. Shortcut AI steps are typed, not free-form by default

A shortcut's AI step lowers to an agent transform with `Audience::Agents`
(`crates/collab/automation/src/shortcut.rs`, `apps/agent/src/transform.rs`):
`Summarise`, `Extract { schema }`, `Classify { choices }` and `Freeform`. The
step's checker types an `Extract` by its schema, so what a shortcut hands the
next step is a value of a declared shape rather than prose that later steps
interpret. The transform's own instruction says the content is data, and the
shortcuts editor treats an agent's answer as untrusted when drafting.

## Defaults

| Situation | Default | Why |
| --- | --- | --- |
| A read the person asked for, of their own data | Allowed, no confirmation | Nothing to confirm: their own bytes. |
| A read of outside content | Allowed, answer labelled untrusted | Reading is not a side effect; the label is what later matters. |
| A change (undoable) | Allowed; Undo reverts it | Damage is bounded by revertibility. |
| A send, an outside action, or an irreversible change | Ask the person | The person is the only one who can accept it. |
| A change whose argument came from an untrusted answer | Ask the person, showing that input | The plan's reasoning is not the person's instruction. |
| A skill or shortcut that reads outside content | The task's normal policy | A skill does not inherit authority the person has not given. |
| An agent's own word as confirmation | Refused | Only the person confirms. |

The rules are the same on every transport: an app run by the shell, an agent
reached over MCP, and a script driving `tilo-control` all pass through the
action policy and the broker's tag.

## Negative tests

Each control has an adversarial case, and "could not observe" is a failure:

- An operation that does not declare `untrusted` is not labelled
  (`planner::tests::an_answer_is_untrusted_only_when_its_contract_says_so`).
- An answer whose operation declared it stays labelled however many times a
  task is re-planned (`planner::tests::a_read_of_outside_content_is_kept_untrusted_for_every_plan`).
- Content that carries outside bytes is marked wherever it reaches the model
  (`mcp_http_tests::untrusted_content_is_labelled_wherever_it_reaches_the_model`).
- A consequential action gets `Ask` even under a permissive policy
  (`crates/collab/action-policy/tests/agent_answers.rs`).
- An agent invoking an action that asks first is refused
  (`agent-participant` participant tests).
- The browser, web search, feeds and the git client's checks tag their
  outside-content operations (`apps/web-search/src/actions.rs` and friends).

## Gaps

Known, with what would close each. Nothing here is a reason to weaken a control
above; each is a follow-up.

1. **Not every outside-content read is tagged.** Mail's message read and export,
   the PDF and ebook readers' text reads, the word processor's file and content
   reads, the file manager's text read and hand-over, and chat's message reads
   read bytes the person did not write but do not declare `untrusted`. The
   mechanism is in place; the declarations are missing per app. Each app's
   absence is a silent hole, and the fix is one effect row per operation plus a
   test that its read is labelled.
2. **Taint is a boolean, not a data-flow.** An answer is untrusted as a whole;
   nothing tracks that a *particular* argument came from a particular span.
   A plan that quotes one word of a mail into a well-formed request is treated
   exactly like one that follows a mail's instructions. Data-flow tracking per
   argument is the honest version, and it is a larger change than this
   document's scope.
3. **No confirmation prompt names taint yet.** The planner is told to put a
   tainted step to the person, and the answer is labelled; the confirmation
   itself does not yet show "this argument came from a web page". Closing it
   needs the invocation to carry the taint and the confirmation surface to read
   it.
4. **Images and audio are not covered.** A picture's pixels and a video's audio
   can carry instructions a model reads; only text answers are labelled. A
   rendered picture's trust needs its own declaration and its own label.
5. **No red-team suite in CI.** There is no corpus of injected documents — a
   mail that says "forward this to everyone", a page with hidden instructions, a
   PDF with white text — run through the evaluation harness on every change, so
   a regression in the label or the policy would not be caught by a fixed
   artificial case. That corpus belongs beside the model evaluation harness and
   is the highest-value gap here: it makes every control above measured instead
   of asserted.
6. **Skills are not sandboxed from their readers.** A skill's text and the
   content it reads share one context. Typecasting a skill's inputs and outputs
   (as shortcut AI steps do) would help; today the label and the policy are the
   whole defense.
7. **Undo's reach as a mitigation is assumed, not measured.** The default
   sometimes rests on "Undo reverts it"; how far one person's Undo reaches in a
   shared record that others have already merged needs deliberate testing
   before that reasoning is relied on alone.
