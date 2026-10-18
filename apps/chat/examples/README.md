# Task conversation acceptance

## Task-first views

The production Chat surface also exposes these explicit capture scenarios:

- `TaskGoalBeforeSetup`, `TaskGoalCompact` (420×680), `TaskGoalFloating`: private goal before worker/model setup.
- `QueuedTask`: an owner-acknowledged saved goal; starting is separate.
- `Subtask`, `TaskTreeCompact` (420×680): hierarchy, public labels and actual child completion counts.
- `RunningSteering`, `TaskConversationFloating`: the same exact task/run editor and controls.
- `TaskAnswer`, `TaskAnswerFloating`: owner-private assistant prose, not public progress summaries.

With an existing matching executable, for example:

```sh
/path/to/chat --shots target/chat-task-shots/goal.png --scenario TaskGoalCompact
/path/to/chat --shots target/chat-task-shots/tree.png --scenario TaskTreeCompact
/path/to/chat --shots target/chat-task-shots/answer.png --scenario TaskAnswerFloating
```

Repeat with `TILO_SHOTS_THEME=light`, `dark`, and `high-contrast`. These commands
are capture instructions, not a claim that screenshots or live native execution
have been verified. Floating scenarios use the existing Conversation surface;
a full desktop-hosted cursor-to-window check additionally needs the native host.

## Offscreen captures

No root-checkout compilation is needed. With an **existing, current-source**
Chat executable and compatible host libraries/fonts:

```sh
CHAT_BINARY=/path/to/chat bash apps/chat/examples/task-conversation.sh
```

Alternatively, set `TILO_CHAT_CAPTURE_IMAGE` to the **existing builder image
used for that executable**. The script uses rootless, network-disabled Podman
without pulling an image. There is deliberately no hardcoded historical CI
image: its fontconfig ABI may not match the executable. Neither path connects
to a compositor or captures the user's desktop; both use `--shots` offscreen.

If the executable is stale, run these commands sequentially in a compatible
build environment. A remote builder can use any cargo drop-in; retrieve the
resulting PNGs through that builder's artifact mechanism.

```sh
cargo run --locked -p chat --bin chat -- --shots target/chat-task-shots/normal.png --scenario agent-task
cargo run --locked -p chat --bin chat -- --shots target/chat-task-shots/compact.png --scenario agent-task-compact
cargo run --locked -p chat --bin chat -- --shots target/chat-task-shots/queued.png --scenario agent-task-queued
cargo run --locked -p chat --bin chat -- --shots target/chat-task-shots/queued-compact.png --scenario agent-task-queued-compact
```

The scenarios draw the production Chat surface at 1280×800 and 420×680,
using the executable's current shared text renderer and installed fonts. Keep
that font environment unchanged when comparing captures. Repeat under light,
dark and high-contrast themes (`TILO_SHOTS_THEME`).

These are **explicit examples**, not evidence of a live model session. The
active fixture has the original instruction, three owner-private assistant reports
(including example action/resource output descriptions), and a saved direction
at revision 6 awaiting application; revision 1 is applied. **Stop**, not
**Start task**, is correct for that active run. The queued fixtures have just
the saved instruction and an inactive epoch-0 frontier: nothing runs until
explicit **Start task** approval.

The fixtures use the actual `read-conversation` turn schema and production
projection. That schema currently exposes public outputs as text; example
output descriptions do not claim clickable resource references or verified
action receipts. Captures do not contact the backend or prove hydration.
Check fresh images for full wrapped task context, legible work attribution,
Saved versus Applied, and composer/control separation at both widths.

The reviewed captures are in `target/chat-avatar-fixed/{dark,light}/`, with
`normal.png`, `compact.png`, `queued.png` and `queued-compact.png` in each
folder. To reproduce one with the portable Cargo interface:

```sh
cargo run --locked -p chat --bin chat -- --shots target/chat-avatar-fixed/light/compact.png --scenario agent-task-compact --theme light
```

Dark normal and light compact show avatars aligned beside their messages,
legible initials and separate composer controls. These are offscreen production
Chat draws, not a mounted desktop or a live model session.

## Live acceptance

Use a development workspace with an enrolled agent and the backend's task
action grants. Open that agent from Chat's Agents section, choose **New task**,
and send a multi-paragraph instruction. Check that the queued task has the full
instruction, not just its bounded title. Queueing saves the task; it must **not**
start execution. Chat follows the queue receipt's exact task into its queued
frontier (run epoch 0 for a new task).

Above the composer, verify **Start task** is offered for the selected queued,
inactive task. Explicitly press Start: Chat invokes the backend's `approve-run`
action with the expected run epoch and revision. **Start approved** means run
approval, not proof of execution. Chat follows the returned new epoch; only the
backend execution frontier may say Accepted, Planning or Working. Start is
also available through the contextual semantic action locus, not a separate
Chat `start-agent-task` resource action.

Once the selected run is active, ask a question or change course: both send
steering to that same run. The editor clears only after an owner receipt;
Saved changes to Applied only after that run reports applying the revision.
Stop reports **Stop requested**, not an assertion that execution has already
ceased. Task/resource outputs and permission policy remain backend-owned.

Close and reopen the exact task/run conversation. Chat now invokes the task
resource's **`read-conversation`** with `run_epoch`: the original full instruction
(epoch 0) and the selected run's durable directions must reappear, attributed
and without duplicates. `read-task` supplies the frontier, not message text.
A stale or mismatched conversation result must never populate another task/run.
Public work updates reported for that run also reappear in chronological order.
They are published execution updates, not hidden reasoning. Private drafts are
not part of this read.

Float and return the conversation using the shared floating host. Verify that
the selected task/run, pending receipt, private draft, caret/selection and IME
remain with the retained editor. With two tasks assigned to the same agent,
explicitly select one before steering; a replacement run must never silently
inherit the old conversation's authority.

The authenticated `chat.agent:<agent>` row exports `open-agent-conversation`
and `open-task-conversation`. The latter asks only for `task` and explicit
`run_epoch` (including zero for a newly queued task); workspace and agent are
bound by Chat, never chosen by that caller. It can follow the exact saved task
before Chat has projected it. Once its queued frontier arrives, the separate
`start-agent-task` effect provides explicit approval. Opening never starts it.
The root also offers exact navigation with all four source/identity fields.
Backend task actions own `queue`, `approve-run`, `direct`, `stop`, and
`read-conversation`; Chat is not an ordinary-message authority lane.

## Focused verification (on the builder)

```sh
cargo test --locked -p chat --lib agent_
cargo test --locked -p tilo-app --lib framework::rows::tests
cargo check --locked -p chat --tests
python3 apps/chat/tests/test_agent_start_surface.py
bash -n apps/chat/examples/task-conversation.sh
```

The focused tests cover full-instruction queueing, receipt parsing,
revision/run fencing, explicit approval without claiming execution, durable
turn hydration/deduplication, and private editor transfer to/from the shared
floating host. Screenshots complement those checks; they do not replace them.
